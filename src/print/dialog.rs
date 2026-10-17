//! The print system's half: the dialog, its preview and the document, drawn
//! with Direct2D (ADR 0036).
//!
//! The dialog is the one `Windows.Graphics.Printing` opens for a window
//! through `IPrintManagerInterop`: the system's own, in the system's language,
//! with the printers, paper and options the driver offers. What it asks of a
//! program is a document source — three COM interfaces on one object — that
//! counts the pages, draws a preview page onto a DXGI surface, and writes the
//! pages for the printer through a Direct2D print control, which turns them
//! into XPS with the picture as a lossless image inside.
//!
//! The print system calls the source from threads of its own, more than one;
//! what the source holds is behind one lock, and the Direct2D factory is the
//! multithreaded one.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::{Context, Result};
use windows::Foundation::{IReference, PropertyValue, TypedEventHandler};
use windows::Graphics::Printing::OptionDetails::{PrintTaskOptionChangedEventArgs, PrintTaskOptionDetails};
use windows::Graphics::Printing::{
    IPrintDocumentSource, IPrintDocumentSource_Impl, PrintManager, PrintOrientation, PrintTaskCompletedEventArgs, PrintTaskCompletion, PrintTaskOptions,
    PrintTaskRequestedEventArgs, PrintTaskSourceRequestedArgs, PrintTaskSourceRequestedHandler, StandardPrintTaskOptions,
};
use windows::Win32::Foundation::{E_FAIL, E_INVALIDARG, E_NOINTERFACE, HMODULE};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_F, D2D_SIZE_U, D2D1_ALPHA_MODE_IGNORE, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_CANNOT_DRAW, D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_OPTIONS_TARGET, D2D1_BITMAP_PROPERTIES1, D2D1_COLOR_SPACE_SRGB,
    D2D1_DEVICE_CONTEXT_OPTIONS_NONE, D2D1_FACTORY_OPTIONS, D2D1_FACTORY_TYPE_MULTI_THREADED, D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
    D2D1_PRINT_CONTROL_PROPERTIES, D2D1_PRINT_FONT_SUBSET_MODE_DEFAULT, D2D1CreateFactory, ID2D1Bitmap1, ID2D1Device, ID2D1DeviceContext, ID2D1Factory1,
};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11CreateDevice, ID3D11Device,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{IDXGIDevice, IDXGISurface};
use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
use windows::Win32::Graphics::Printing::{FinalPageCount, IPrintPreviewDxgiPackageTarget};
use windows::Win32::Storage::Packaging::Opc::IOpcPartUri;
use windows::Win32::Storage::Xps::Printing::{ID_DOCUMENTPACKAGETARGET_MSXPS, IPrintDocumentPackageTarget, IPrintDocumentPackageTarget_Impl};
use windows::Win32::Storage::Xps::{
    IXpsDocumentPackageTarget, IXpsDocumentPackageTarget_Impl, IXpsOMObjectFactory, IXpsOMPackageWriter, XPS_DOCUMENT_TYPE, XPS_DOCUMENT_TYPE_XPS,
    XPS_INTERLEAVING_OFF, XpsOMObjectFactory,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemAlloc, IStream, STGM_CREATE, STGM_SHARE_EXCLUSIVE, STGM_WRITE};
use windows::Win32::System::WinRT::Printing::{
    IPrintDocumentPageSource, IPrintDocumentPageSource_Impl, IPrintManagerInterop, IPrintPreviewPageCollection, IPrintPreviewPageCollection_Impl,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::SHCreateStreamOnFileEx;
use windows::core::{AgileReference, GUID, HSTRING, IInspectable, IUnknownImpl, Interface, Ref, implement};

use super::{Area, Fit, Job, Outcome, PIXELS_PER_INCH, Page, Paper, Placement, UNITS_PER_INCH, place, sent};
use crate::com::Com;

/// The page's own option, as the print system and the dialog know it.
const FIT_OPTION: &str = "nitid.fit";
const WHOLE: &str = "whole";
const FILLING: &str = "filling";

/// The print system's way of asking for the page the person is looking at.
const CURRENT_PAGE: u32 = u32::MAX;

/// The choice between whole and filling, kept while the viewer runs: a
/// person printing a stack of pictures chooses once. Not a setting — the
/// next time the viewer starts, a page starts whole again, the one choice
/// that never cuts anything off.
static CHOSEN: Mutex<Fit> = Mutex::new(Fit::Fit);

/// Who hears what came of a print.
type Told = Arc<Mutex<Box<dyn Fn(Outcome) + Send>>>;

/// Open the print dialog for `job`, over `window`.
///
/// Returns as soon as the dialog is up; `told` hears what came of it, from
/// one of the print system's threads, once the person prints. Closing the
/// dialog without printing is not news and is not told.
pub fn show(window: &winit::window::Window, job: Job, told: impl Fn(Outcome) + Send + 'static) -> Result<()> {
    let hwnd = crate::com::hwnd_of(window)?;
    let _com = Com::begin();

    let interop = windows::core::factory::<PrintManager, IPrintManagerInterop>().context("reaching the print system")?;
    // SAFETY: `hwnd` is the live window this call was made from.
    let manager: PrintManager = unsafe { interop.GetForWindow(hwnd) }.context("reaching the print system for this window")?;

    // SAFETY: as above.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    let landscape = job.landscape();
    let title = HSTRING::from(job.title.as_str());
    let shared = Arc::new(Shared {
        job,
        paper: OnceLock::new(),
        preview_dpi: if dpi > 0 { dpi as f32 } else { 96.0 },
        state: Locked(Mutex::new(State::default())),
    });
    let source: IPrintDocumentSource = Document { shared: Arc::clone(&shared) }.into();
    let source = AgileReference::new(&source)?;
    let told: Told = Arc::new(Mutex::new(Box::new(told)));

    // One dialog, one request: the handler takes itself off when it is
    // called, so the next Ctrl+P registers afresh with the next picture
    // rather than finding this one.
    let registration: Arc<Mutex<Option<i64>>> = Arc::default();
    let handler = {
        let registration = Arc::clone(&registration);
        TypedEventHandler::<PrintManager, PrintTaskRequestedEventArgs>::new(move |manager, args| {
            if let (Some(manager), Some(token)) = (manager.as_ref(), registration.lock().ok().and_then(|mut token| token.take())) {
                let _ = manager.RemovePrintTaskRequested(token);
            }
            request(args.ok()?, &title, &source, &shared, landscape, &told)
        })
    };
    let token = manager.PrintTaskRequested(&handler).context("asking the print system for a dialog")?;
    if let Ok(mut slot) = registration.lock() {
        *slot = Some(token);
    }

    // The operation is not waited on: the dialog belongs to the system, and
    // the window keeps drawing while it is up.
    // SAFETY: as above.
    if let Err(error) = unsafe { interop.ShowPrintUIForWindowAsync::<IInspectable>(hwnd) } {
        if let Some(token) = registration.lock().ok().and_then(|mut token| token.take()) {
            let _ = manager.RemovePrintTaskRequested(token);
        }
        return Err(error).context("opening the print dialog");
    }
    Ok(())
}

/// Write the page `job` makes to an XPS file rather than to a printer.
///
/// The document a printer would be handed, through the same drawing — for a
/// gate that reads it back where there is no printer at all.
pub fn write_xps(job: &Job, page: Page, fit: Fit, path: &Path) -> Result<()> {
    let _com = Com::begin();
    let paper = Paper::new(job)?;
    let graphics = Graphics::new()?;
    // SAFETY: plain COM creation on a thread with COM begun above.
    let factory: IXpsOMObjectFactory = unsafe { CoCreateInstance(&XpsOMObjectFactory, None, CLSCTX_INPROC_SERVER) }.context("reaching XPS")?;
    let stream = file_stream(path)?;
    let target: IPrintDocumentPackageTarget = XpsFile { factory, stream }.into();
    let placement = place((paper.full.width, paper.full.height), &page, fit);
    write_page(&graphics, &target, &page, &placement, &paper.full)
}

/// Answer the dialog's request: the task, its options, and where its pages
/// come from.
fn request(
    args: &PrintTaskRequestedEventArgs,
    title: &HSTRING,
    source: &AgileReference<IPrintDocumentSource>,
    shared: &Arc<Shared>,
    landscape: bool,
    told: &Told,
) -> windows::core::Result<()> {
    let source = source.clone();
    let task = args.Request()?.CreatePrintTask(
        title,
        &PrintTaskSourceRequestedHandler::new(move |args: Ref<PrintTaskSourceRequestedArgs>| args.ok()?.SetSource(&source.resolve()?)),
    )?;

    let options = task.Options()?;
    options.SetOrientation(if landscape { PrintOrientation::Landscape } else { PrintOrientation::Portrait })?;

    // What the dialog shows, in this order. An option the printer does not
    // have — copies on a printer to PDF, borders on one that cannot print
    // without them — is left out by the dialog itself.
    let details = PrintTaskOptionDetails::GetFromPrintTaskOptions(&options)?;
    let shown = details.DisplayedOptions()?;
    shown.Clear()?;
    shown.Append(&StandardPrintTaskOptions::Copies()?)?;
    shown.Append(&StandardPrintTaskOptions::Orientation()?)?;

    let fit = details.CreateItemListOption(&HSTRING::from(FIT_OPTION), &HSTRING::from("Picture"))?;
    fit.AddItem(&HSTRING::from(WHOLE), &HSTRING::from("Whole on the page"))?;
    fit.AddItem(&HSTRING::from(FILLING), &HSTRING::from("Filling the page"))?;
    let chosen = CHOSEN.lock().map(|chosen| *chosen).unwrap_or(Fit::Fit);
    fit.TrySetValue(&PropertyValue::CreateString(&HSTRING::from(key(chosen)))?)?;
    shown.Append(&HSTRING::from(FIT_OPTION))?;

    for option in [
        StandardPrintTaskOptions::MediaSize()?,
        StandardPrintTaskOptions::MediaType()?,
        StandardPrintTaskOptions::Bordering()?,
        StandardPrintTaskOptions::ColorMode()?,
        StandardPrintTaskOptions::PrintQuality()?,
    ] {
        shown.Append(&option)?;
    }

    // The print system redraws the preview by itself when one of its own
    // options changes; for this one it has to be told.
    let changed = Arc::clone(shared);
    details.OptionChanged(&TypedEventHandler::<PrintTaskOptionDetails, PrintTaskOptionChangedEventArgs>::new(
        move |details, args| {
            let (Some(details), Some(args)) = (details.as_ref(), args.as_ref()) else {
                return Ok(());
            };
            if text_of(&args.OptionId()?).as_deref() != Some(FIT_OPTION) {
                return Ok(());
            }
            if let Some(fit) = fit_in(details)
                && let Ok(mut chosen) = CHOSEN.lock()
            {
                *chosen = fit;
            }
            changed.invalidate()
        },
    ))?;

    let told = Arc::clone(told);
    task.Completed(&TypedEventHandler::<windows::Graphics::Printing::PrintTask, PrintTaskCompletedEventArgs>::new(
        move |_, args| {
            let outcome = match args.ok()?.Completion()? {
                PrintTaskCompletion::Submitted => Outcome::Sent,
                PrintTaskCompletion::Failed => Outcome::Failed,
                // Cancelled or abandoned: the person closed the dialog.
                _ => return Ok(()),
            };
            if let Ok(told) = told.lock() {
                told(outcome);
            }
            Ok(())
        },
    ))?;
    Ok(())
}

/// The option's key for a choice.
fn key(fit: Fit) -> &'static str {
    match fit {
        Fit::Fit => WHOLE,
        Fit::Fill => FILLING,
    }
}

/// The text a boxed value carries, when it carries text.
fn text_of(value: &IInspectable) -> Option<String> {
    value.cast::<IReference<HSTRING>>().ok()?.Value().ok().map(|text| text.to_string())
}

/// The choice the dialog holds for the page's own option.
fn fit_in(details: &PrintTaskOptionDetails) -> Option<Fit> {
    let option = details.Options().ok()?.Lookup(&HSTRING::from(FIT_OPTION)).ok()?;
    match text_of(&option.Value().ok()?)?.as_str() {
        FILLING => Some(Fit::Fill),
        WHOLE => Some(Fit::Fit),
        _ => None,
    }
}

/// The choice, read from the options the print system hands the source.
fn fit_of(options: &PrintTaskOptions) -> Fit {
    PrintTaskOptionDetails::GetFromPrintTaskOptions(options)
        .ok()
        .and_then(|details| fit_in(&details))
        .unwrap_or_else(|| CHOSEN.lock().map(|chosen| *chosen).unwrap_or(Fit::Fit))
}

/// The sheet the print system describes for a page.
fn page_of(options: &PrintTaskOptions, number: u32) -> windows::core::Result<Page> {
    let description = options.GetPageDescription(number)?;
    Ok(Page {
        width: f64::from(description.PageSize.Width),
        height: f64::from(description.PageSize.Height),
        printable: Area {
            x: f64::from(description.ImageableRect.X),
            y: f64::from(description.ImageableRect.Y),
            width: f64::from(description.ImageableRect.Width),
            height: f64::from(description.ImageableRect.Height),
        },
    })
}

/// What the document source shares with the handlers the dialog calls.
struct Shared {
    job: Job,
    /// The picture made ready for paper, the first time anything asks: on one
    /// of the print system's threads while the dialog shows its own spinner,
    /// never on the viewer's.
    paper: OnceLock<Result<Paper, String>>,
    /// The window's dots to the inch, which the preview is drawn at so it is
    /// sharp on a scaled display.
    preview_dpi: f32,
    state: Locked,
}

impl Shared {
    fn paper(&self) -> windows::core::Result<&Paper> {
        self.paper
            .get_or_init(|| Paper::new(&self.job).map_err(|error| format!("{error:#}")))
            .as_ref()
            .map_err(|error| failure(error))
    }

    /// Have the preview drawn again.
    fn invalidate(&self) -> windows::core::Result<()> {
        let preview = self.state.lock()?.preview.clone();
        match preview {
            // SAFETY: a target the print system handed this source.
            Some(preview) => unsafe { preview.InvalidatePreview() },
            None => Ok(()),
        }
    }
}

/// The objects the print system and Direct2D lend the source.
#[derive(Default)]
struct State {
    graphics: Option<Graphics>,
    preview: Option<IPrintPreviewDxgiPackageTarget>,
    /// The sheet and the choice as last counted, for the preview to draw.
    page: Option<(Page, Fit)>,
}

/// [`State`] behind its lock.
struct Locked(Mutex<State>);

// SAFETY: what `State` holds is the print system's preview target and
// Direct3D, Direct2D and WIC objects made by a multithreaded factory; all of
// them may be called from any thread, and every call goes through the lock.
unsafe impl Send for Locked {}
// SAFETY: as above.
unsafe impl Sync for Locked {}

impl Locked {
    fn lock(&self) -> windows::core::Result<std::sync::MutexGuard<'_, State>> {
        self.0.lock().map_err(|_| failure("an earlier print call failed while drawing"))
    }
}

/// An error the print system can carry, said for the terminal as well.
fn failure(message: &str) -> windows::core::Error {
    eprintln!("nitid: printing: {message}");
    windows::core::Error::new(E_FAIL, message)
}

/// The document source the dialog draws its preview from and prints.
#[implement(IPrintDocumentSource, IPrintDocumentPageSource, IPrintPreviewPageCollection)]
struct Document {
    shared: Arc<Shared>,
}

impl IPrintDocumentSource_Impl for Document_Impl {}

impl IPrintDocumentPageSource_Impl for Document_Impl {
    fn GetPreviewPageCollection(&self, target: Ref<IPrintDocumentPackageTarget>) -> windows::core::Result<IPrintPreviewPageCollection> {
        // SAFETY: the preview target is the one documented type this
        // document target carries for previews; its IID is its type's.
        let preview: IPrintPreviewDxgiPackageTarget = unsafe { target.ok()?.GetPackageTarget(&IPrintPreviewDxgiPackageTarget::IID)? };
        self.shared.state.lock()?.preview = Some(preview);
        Ok(self.to_interface())
    }

    fn MakeDocument(&self, options: Ref<IInspectable>, target: Ref<IPrintDocumentPackageTarget>) -> windows::core::Result<()> {
        let options: PrintTaskOptions = options.ok()?.cast()?;
        let page = page_of(&options, 1)?;
        let fit = fit_of(&options);
        let paper = self.shared.paper()?;
        let placement = place((paper.full.width, paper.full.height), &page, fit);

        let mut state = self.shared.state.lock()?;
        let graphics = graphics_in(&mut state)?;
        write_page(graphics, target.ok()?, &page, &placement, &paper.full).map_err(|error| failure(&format!("{error:#}")))
    }
}

impl IPrintPreviewPageCollection_Impl for Document_Impl {
    fn Paginate(&self, current: u32, options: Ref<IInspectable>) -> windows::core::Result<()> {
        let options: PrintTaskOptions = options.ok()?.cast()?;
        let number = if current == CURRENT_PAGE || current == 0 { 1 } else { current };
        let page = page_of(&options, number)?;
        let fit = fit_of(&options);

        let preview = {
            let mut state = self.shared.state.lock()?;
            state.page = Some((page, fit));
            state.preview.clone()
        };
        let preview = preview.ok_or_else(|| failure("the preview was asked for before its target"))?;
        // One picture, one page.
        // SAFETY: a target the print system handed this source.
        unsafe {
            preview.SetJobPageCount(FinalPageCount, 1)?;
            preview.InvalidatePreview()
        }
    }

    fn MakePage(&self, desired: u32, width: f32, height: f32) -> windows::core::Result<()> {
        if !(width > 0.0 && height > 0.0) {
            return Err(E_INVALIDARG.into());
        }
        let number = if desired == CURRENT_PAGE { 1 } else { desired };
        let paper = self.shared.paper()?;

        let mut state = self.shared.state.lock()?;
        let (page, fit) = state
            .page
            .ok_or_else(|| failure("a preview page was asked for before the pages were counted"))?;
        let preview = state.preview.clone().ok_or_else(|| failure("a preview page was asked for before its target"))?;
        let dpi = self.shared.preview_dpi;
        let graphics = graphics_in(&mut state)?;

        let placement = place((paper.full.width, paper.full.height), &page, fit);
        // The sheet scaled into the space the dialog gives the preview.
        let scale = f64::from(width / page.width as f32).min(f64::from(height / page.height as f32));
        let per_unit = scale * f64::from(dpi) / UNITS_PER_INCH;
        let pixels = sent(&paper.preview, &placement, per_unit);
        preview_page(graphics, &preview, number, (width, height), dpi, scale as f32, &placement, &pixels).map_err(|error| failure(&format!("{error:#}")))
    }
}

/// The Direct2D device the source draws with, made the first time it is
/// needed.
fn graphics_in(state: &mut State) -> windows::core::Result<&Graphics> {
    if state.graphics.is_none() {
        state.graphics = Some(Graphics::new().map_err(|error| failure(&format!("{error:#}")))?);
    }
    Ok(state.graphics.as_ref().expect("made just above"))
}

/// Direct3D for the preview's surfaces, Direct2D to draw, WIC for the print
/// control to encode the picture with.
struct Graphics {
    d3d: ID3D11Device,
    device: ID2D1Device,
    wic: IWICImagingFactory,
}

impl Graphics {
    fn new() -> Result<Self> {
        // A hardware device where there is one, the software rasteriser where
        // there is not — a remote session, a test machine. A page is small
        // work for either.
        let d3d = d3d_device(D3D_DRIVER_TYPE_HARDWARE).or_else(|_| d3d_device(D3D_DRIVER_TYPE_WARP))?;
        let dxgi: IDXGIDevice = d3d.cast()?;
        // SAFETY: plain factory and device creation.
        let factory: ID2D1Factory1 =
            unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_MULTI_THREADED, Some(&D2D1_FACTORY_OPTIONS::default())) }.context("starting Direct2D")?;
        // SAFETY: as above.
        let device = unsafe { factory.CreateDevice(&dxgi) }.context("starting Direct2D on the device")?;
        // SAFETY: as above, on a thread with COM.
        let wic: IWICImagingFactory = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }.context("reaching WIC")?;
        Ok(Self { d3d, device, wic })
    }
}

fn d3d_device(kind: D3D_DRIVER_TYPE) -> Result<ID3D11Device> {
    let mut device = None;
    // SAFETY: the out-pointer is a local the call fills.
    unsafe {
        D3D11CreateDevice(
            None,
            kind,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    }
    .context("starting Direct3D")?;
    device.context("Direct3D started without a device")
}

/// The picture as a Direct2D bitmap, at 96 dots to the inch so one pixel is
/// one unit of the page before the placement scales it.
fn bitmap(context: &ID2D1DeviceContext, pixels: &crate::image_source::DecodedImage) -> Result<ID2D1Bitmap1> {
    let properties = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            // Opaque: the picture was laid on white already.
            alphaMode: D2D1_ALPHA_MODE_IGNORE,
        },
        dpiX: UNITS_PER_INCH as f32,
        dpiY: UNITS_PER_INCH as f32,
        bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
        colorContext: std::mem::ManuallyDrop::new(None),
    };
    // SAFETY: `pixels` holds `width * height` tightly packed four-byte
    // pixels, which is what the pitch says.
    unsafe {
        context.CreateBitmap(
            D2D_SIZE_U {
                width: pixels.width,
                height: pixels.height,
            },
            Some(pixels.pixels.as_ptr().cast()),
            pixels.width * 4,
            &properties,
        )
    }
    .context("handing the picture to Direct2D")
}

fn rect(area: &Area) -> D2D_RECT_F {
    D2D_RECT_F {
        left: area.x as f32,
        top: area.y as f32,
        right: (area.x + area.width) as f32,
        bottom: (area.y + area.height) as f32,
    }
}

/// Write the page for the printer.
fn write_page(
    graphics: &Graphics,
    target: &IPrintDocumentPackageTarget,
    page: &Page,
    placement: &Placement,
    paper: &crate::image_source::DecodedImage,
) -> Result<()> {
    let pixels = sent(paper, placement, PIXELS_PER_INCH / UNITS_PER_INCH);
    let properties = D2D1_PRINT_CONTROL_PROPERTIES {
        fontSubset: D2D1_PRINT_FONT_SUBSET_MODE_DEFAULT,
        // What Direct2D rasterises itself, which on this page is nothing: the
        // picture travels as an image at its own pixels.
        rasterDPI: PIXELS_PER_INCH as f32,
        colorSpace: D2D1_COLOR_SPACE_SRGB,
    };
    // SAFETY: Direct2D calls on objects made above, in the order its print
    // control documents: draw into a command list, add it as a page, close.
    unsafe {
        let control = graphics
            .device
            .CreatePrintControl(&graphics.wic, target, Some(&properties))
            .context("starting the print")?;
        let context = graphics.device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let list = context.CreateCommandList()?;
        context.SetTarget(&list);
        context.BeginDraw();
        // No white is painted: the paper is white, and a sheet-sized fill
        // would be ink, or at least work, for nothing.
        let drawn = bitmap(&context, &pixels);
        if let Ok(drawn) = &drawn {
            context.DrawBitmap(
                drawn,
                Some(&rect(&placement.on_page)),
                1.0,
                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                None,
                None,
            );
        }
        context.EndDraw(None, None).context("drawing the page")?;
        drawn?;
        list.Close()?;
        control
            .AddPage(
                &list,
                D2D_SIZE_F {
                    width: page.width as f32,
                    height: page.height as f32,
                },
                None,
                None,
                None,
            )
            .context("adding the page")?;
        control.Close().context("finishing the print")?;
    }
    Ok(())
}

/// Draw the preview of a page onto a surface of its own and hand it over.
#[allow(clippy::too_many_arguments)]
fn preview_page(
    graphics: &Graphics,
    preview: &IPrintPreviewDxgiPackageTarget,
    number: u32,
    size: (f32, f32),
    dpi: f32,
    scale: f32,
    placement: &Placement,
    pixels: &crate::image_source::DecodedImage,
) -> Result<()> {
    let description = D3D11_TEXTURE2D_DESC {
        Width: (size.0 * dpi / UNITS_PER_INCH as f32).ceil().max(1.0) as u32,
        Height: (size.1 * dpi / UNITS_PER_INCH as f32).ceil().max(1.0) as u32,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    // SAFETY: Direct3D and Direct2D calls on objects made above; a fresh
    // device context for each page, because a context holds drawing state
    // and the print system may ask for two pages at once.
    unsafe {
        let mut texture = None;
        graphics
            .d3d
            .CreateTexture2D(&description, None, Some(&mut texture))
            .context("making the preview's surface")?;
        let surface: IDXGISurface = texture.context("Direct3D made no surface")?.cast()?;
        let context = graphics.device.CreateDeviceContext(D2D1_DEVICE_CONTEXT_OPTIONS_NONE)?;
        let properties = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT {
                format: DXGI_FORMAT_B8G8R8A8_UNORM,
                alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
            },
            dpiX: dpi,
            dpiY: dpi,
            bitmapOptions: D2D1_BITMAP_OPTIONS_TARGET | D2D1_BITMAP_OPTIONS_CANNOT_DRAW,
            colorContext: std::mem::ManuallyDrop::new(None),
        };
        let target = context.CreateBitmapFromDxgiSurface(&surface, Some(&properties))?;
        context.SetTarget(&target);
        // The context's own dots to the inch, not the target's: a context
        // draws at 96 until told otherwise, which on a window at 200% put the
        // page in the top-left quarter of its preview.
        context.SetDpi(dpi, dpi);
        context.BeginDraw();
        context.Clear(Some(&D2D1_COLOR_F {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        }));
        context.SetTransform(&windows_numerics::Matrix3x2 {
            M11: scale,
            M12: 0.0,
            M21: 0.0,
            M22: scale,
            M31: 0.0,
            M32: 0.0,
        });
        let drawn = bitmap(&context, pixels);
        if let Ok(drawn) = &drawn {
            context.DrawBitmap(
                drawn,
                Some(&rect(&placement.on_page)),
                1.0,
                D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                None,
                None,
            );
        }
        context.EndDraw(None, None).context("drawing the preview")?;
        drawn?;
        preview.DrawPage(number, &surface, dpi, dpi).context("handing the preview over")?;
    }
    Ok(())
}

/// A file to write XPS into.
fn file_stream(path: &Path) -> Result<IStream> {
    let wide = HSTRING::from(path.as_os_str());
    // SAFETY: creates or truncates the named file; the attribute is
    // FILE_ATTRIBUTE_NORMAL.
    unsafe { SHCreateStreamOnFileEx(&wide, (STGM_CREATE | STGM_WRITE | STGM_SHARE_EXCLUSIVE).0, 0x80, true, None) }
        .with_context(|| format!("creating {}", path.display()))
}

/// A document target that writes XPS into a file: what a printer's queue is
/// to the print control, with no queue — for [`write_xps`].
#[implement(IPrintDocumentPackageTarget, IXpsDocumentPackageTarget)]
struct XpsFile {
    factory: IXpsOMObjectFactory,
    stream: IStream,
}

impl IPrintDocumentPackageTarget_Impl for XpsFile_Impl {
    fn GetPackageTargetTypes(&self, count: *mut u32, kinds: *mut *mut GUID) -> windows::core::Result<()> {
        // SAFETY: the caller owns the array and frees it with CoTaskMemFree,
        // as the interface documents.
        unsafe {
            let array = CoTaskMemAlloc(std::mem::size_of::<GUID>()).cast::<GUID>();
            if array.is_null() {
                return Err(E_FAIL.into());
            }
            array.write(ID_DOCUMENTPACKAGETARGET_MSXPS);
            kinds.write(array);
            count.write(1);
        }
        Ok(())
    }

    fn GetPackageTarget(&self, kind: *const GUID, interface: *const GUID, target: *mut *mut core::ffi::c_void) -> windows::core::Result<()> {
        // SAFETY: pointers from the caller, valid for the call.
        if unsafe { *kind } != ID_DOCUMENTPACKAGETARGET_MSXPS {
            return Err(E_NOINTERFACE.into());
        }
        let xps: IXpsDocumentPackageTarget = self.to_interface();
        // SAFETY: as above.
        unsafe { xps.query(interface, target).ok() }
    }

    fn Cancel(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

impl IXpsDocumentPackageTarget_Impl for XpsFile_Impl {
    fn GetXpsOMPackageWriter(&self, sequence: Ref<IOpcPartUri>, discard: Ref<IOpcPartUri>) -> windows::core::Result<IXpsOMPackageWriter> {
        // SAFETY: the stream and the part names are live for the call.
        unsafe {
            self.factory
                .CreatePackageWriterOnStream(&self.stream, true, XPS_INTERLEAVING_OFF, sequence.as_ref(), None, None, None, discard.as_ref())
        }
    }

    fn GetXpsOMFactory(&self) -> windows::core::Result<IXpsOMObjectFactory> {
        Ok(self.factory.clone())
    }

    fn GetXpsType(&self) -> windows::core::Result<XPS_DOCUMENT_TYPE> {
        Ok(XPS_DOCUMENT_TYPE_XPS)
    }
}
