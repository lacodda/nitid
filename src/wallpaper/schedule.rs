//! The schedule: one task in the Windows Task Scheduler, named
//! `nitid wallpaper`, that runs `nitidw.exe wallpaper next` (ADR 0035).
//!
//! The task is written as the Task Scheduler's own XML and registered whole,
//! because the settings that matter are ones the `schtasks` command line
//! cannot reach: by default a task does not start on battery and stops when
//! the power cable comes out, which on a laptop would be a desktop that
//! changes only at a desk.

use std::path::Path;
use std::time::{Duration, SystemTime};

use anyhow::Result;

use super::Every;

/// Make the task, or remake it with a new interval. The first run is one
/// interval from now: the picture was changed just before this is called.
pub fn register(every: Every, program: &Path) -> Result<()> {
    let first = SystemTime::now() + Duration::from_secs(u64::from(every.in_minutes()) * 60);
    imp::register(&task_xml(every, program, first))
}

/// Take the task away. False when there was none.
pub fn remove() -> Result<bool> {
    imp::remove()
}

/// Whether the task is there.
pub fn registered() -> Result<bool> {
    imp::registered()
}

/// When the task next runs, in local time, written for a person; `None` when
/// the Task Scheduler does not say.
pub fn next_run() -> Result<Option<String>> {
    Ok(imp::next_run()?.map(local_date_text))
}

/// The task, as the Task Scheduler's XML.
///
/// What each setting is for:
/// - a time trigger from `first`, repeating every interval for ever;
/// - run as the person, only while they are signed in — the desktop is theirs,
///   and a task in another session would change a desktop nobody sees;
/// - start on battery and keep going on battery;
/// - a run missed while the machine slept happens once when it wakes;
/// - a run still going when the next is due is left to finish, not doubled;
/// - five minutes at most: preparing even a very large picture takes seconds,
///   and a run that hangs must not hold the schedule for ever.
fn task_xml(every: Every, program: &Path, first: SystemTime) -> String {
    format!(
        r#"<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Author>nitid</Author>
    <Description>Changes the desktop's picture from a folder every {words}. Made by nitid wallpaper; nitid wallpaper off removes it.</Description>
  </RegistrationInfo>
  <Triggers>
    <TimeTrigger>
      <Repetition>
        <Interval>{interval}</Interval>
        <StopAtDurationEnd>false</StopAtDurationEnd>
      </Repetition>
      <StartBoundary>{start}</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <IdleSettings>
      <StopOnIdleEnd>false</StopOnIdleEnd>
      <RestartOnIdle>false</RestartOnIdle>
    </IdleSettings>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <Hidden>false</Hidden>
    <RunOnlyIfIdle>false</RunOnlyIfIdle>
    <WakeToRun>false</WakeToRun>
    <ExecutionTimeLimit>PT5M</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>"{program}"</Command>
      <Arguments>wallpaper next</Arguments>
    </Exec>
  </Actions>
</Task>
"#,
        words = escape(&every.text()),
        interval = every.duration(),
        start = utc_text(first),
        program = escape(&program.display().to_string()),
    )
}

/// Text made safe to stand inside an XML element.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// A moment as the Task Scheduler reads one in UTC: `2026-10-10T12:30:00Z`.
///
/// UTC rather than local time, so no time-zone table is needed to write it;
/// the trigger then runs on the same instants wherever the clock is set.
fn utc_text(moment: SystemTime) -> String {
    let seconds = moment.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |elapsed| elapsed.as_secs());
    let (year, month, day) = civil(seconds.div_euclid(86_400) as i64);
    let time = seconds % 86_400;
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", time / 3600, time / 60 % 60, time % 60)
}

/// An OLE date — days since the end of 1899, in local time, which is how the
/// Task Scheduler answers "when next" — as `2026-10-10 15:30`.
fn local_date_text(date: f64) -> String {
    // 25569 days from the OLE epoch to 1970-01-01.
    let seconds = ((date - 25_569.0) * 86_400.0).round() as i64;
    let (year, month, day) = civil(seconds.div_euclid(86_400));
    let time = seconds.rem_euclid(86_400);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}", time / 3600, time / 60 % 60)
}

/// The calendar date `days` after 1970-01-01, by Howard Hinnant's algorithm —
/// a dozen lines of arithmetic, where a date crate would be a dependency for
/// two formatted strings.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(windows)]
mod imp {
    use anyhow::{Context, Result};
    use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
    use windows::Win32::System::TaskScheduler::{ITaskFolder, ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN, TaskScheduler};
    use windows::Win32::System::Variant::VARIANT;
    use windows::core::{BSTR, HRESULT};

    use crate::com::Com;

    /// The task's name, in the root of the Task Scheduler's library — where a
    /// person looking for it in the Task Scheduler window finds it first.
    /// Here rather than beside the XML because only this half names it: the
    /// stand-in elsewhere has no library to look in.
    const TASK: &str = "nitid wallpaper";

    /// The root of the Task Scheduler's library, with COM held for as long as
    /// it is. The fields drop in order: the folder before COM goes.
    struct Library {
        root: ITaskFolder,
        _com: Com,
    }

    fn library() -> Result<Library> {
        let com = Com::begin();
        // SAFETY: COM is initialised on this thread for the life of `com`,
        // which the returned value keeps.
        let service: ITaskService = unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }.context("reaching the Task Scheduler")?;
        let empty = VARIANT::default();
        // Empty everything: this machine, as the person running this.
        unsafe { service.Connect(&empty, &empty, &empty, &empty) }.context("connecting to the Task Scheduler")?;
        let root = unsafe { service.GetFolder(&BSTR::from("\\")) }.context("opening the Task Scheduler's library")?;
        Ok(Library { root, _com: com })
    }

    /// "There is no such task", as the Task Scheduler says it.
    fn not_found(error: &windows::core::Error) -> bool {
        error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
    }

    pub fn register(xml: &str) -> Result<()> {
        let library = library()?;
        let empty = VARIANT::default();
        // SAFETY: a live folder interface and owned strings for the call.
        unsafe {
            library.root.RegisterTask(
                &BSTR::from(TASK),
                &BSTR::from(xml),
                TASK_CREATE_OR_UPDATE.0,
                &empty,
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )
        }
        .context("making the scheduled task")?;
        Ok(())
    }

    pub fn remove() -> Result<bool> {
        let library = library()?;
        // SAFETY: as above.
        match unsafe { library.root.DeleteTask(&BSTR::from(TASK), 0) } {
            Ok(()) => Ok(true),
            Err(error) if not_found(&error) => Ok(false),
            Err(error) => Err(error).context("removing the scheduled task"),
        }
    }

    pub fn registered() -> Result<bool> {
        let library = library()?;
        // SAFETY: as above.
        match unsafe { library.root.GetTask(&BSTR::from(TASK)) } {
            Ok(_) => Ok(true),
            Err(error) if not_found(&error) => Ok(false),
            Err(error) => Err(error).context("reading the scheduled task"),
        }
    }

    pub fn next_run() -> Result<Option<f64>> {
        let library = library()?;
        // SAFETY: as above.
        let task = match unsafe { library.root.GetTask(&BSTR::from(TASK)) } {
            Ok(task) => task,
            Err(error) if not_found(&error) => return Ok(None),
            Err(error) => return Err(error).context("reading the scheduled task"),
        };
        // Zero is the Task Scheduler's "not scheduled".
        Ok(unsafe { task.NextRunTime() }.ok().filter(|date| *date > 0.0))
    }
}

/// Elsewhere there is no Task Scheduler. Asking whether a task is there is
/// answered — there is none — so the viewer and `nitid wallpaper` can say so;
/// asking for one to be made is refused.
#[cfg(not(windows))]
mod imp {
    use anyhow::{Result, bail};

    pub fn register(_xml: &str) -> Result<()> {
        bail!("the desktop's picture is changed on a schedule on Windows only")
    }

    pub fn remove() -> Result<bool> {
        Ok(false)
    }

    pub fn registered() -> Result<bool> {
        Ok(false)
    }

    pub fn next_run() -> Result<Option<f64>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_written_on_the_calendar() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(-1), (1969, 12, 31));
        assert_eq!(civil(11_016), (2000, 2, 29), "a leap day in a century leap year");
        assert_eq!(civil(20_736), (2026, 10, 10));
        let moment = SystemTime::UNIX_EPOCH + Duration::from_secs(20_736 * 86_400 + 12 * 3600 + 30 * 60 + 5);
        assert_eq!(utc_text(moment), "2026-10-10T12:30:05Z");
        // 2026-10-10 is OLE day 46305; a quarter past six in the evening.
        assert_eq!(local_date_text(46_305.0 + 18.25 / 24.0), "2026-10-10 18:15");
    }

    /// The task holds every setting the schedule depends on — and the ones
    /// a default task gets wrong on a laptop above all.
    #[test]
    fn the_task_says_everything_the_schedule_depends_on() {
        let first = SystemTime::UNIX_EPOCH + Duration::from_secs(20_736 * 86_400);
        let xml = task_xml(Every::minutes(90), Path::new(r"C:\Users\A & B\nitid\nitidw.exe"), first);

        for wanted in [
            "<Interval>PT1H30M</Interval>",
            "<StartBoundary>2026-10-10T00:00:00Z</StartBoundary>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            "<StartWhenAvailable>true</StartWhenAvailable>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<LogonType>InteractiveToken</LogonType>",
            "<RunLevel>LeastPrivilege</RunLevel>",
            "<Arguments>wallpaper next</Arguments>",
            // The ampersand in the path is escaped, or the XML is refused.
            r#"<Command>"C:\Users\A &amp; B\nitid\nitidw.exe"</Command>"#,
        ] {
            assert!(xml.contains(wanted), "the task does not say {wanted}:\n{xml}");
        }
        assert!(!xml.contains("A & B"), "an unescaped ampersand went into the XML");
    }
}
