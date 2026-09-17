// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

// A github.io project site, so the pages are served under a base path. Moving
// to a domain of its own means a CNAME in ./public, `site` set to that domain
// and no `base` at all - the two arrangements do not mix.
export default defineConfig({
	site: 'https://lacodda.github.io',
	base: '/nitid',
	integrations: [
		starlight({
			title: 'nitid',
			description:
				'A fast image viewer for Windows with honest color and HDR: first pixels in milliseconds, ICC on the GPU, modern formats.',
			logo: {
				src: './src/assets/logo.svg',
				alt: 'nitid',
			},
			favicon: '/favicon.svg',
			customCss: ['./src/styles/brand.css'],
			head: [
				{ tag: 'link', attrs: { rel: 'apple-touch-icon', href: '/nitid/apple-touch-icon.png' } },
				{
					tag: 'meta',
					attrs: { property: 'og:image', content: 'https://raw.githubusercontent.com/lacodda/nitid/main/assets/social-preview.png' },
				},
				{ tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' } },
			],
			social: [{ icon: 'github', label: 'GitHub', href: 'https://github.com/lacodda/nitid' }],
			editLink: {
				baseUrl: 'https://github.com/lacodda/nitid/edit/main/docs/',
			},
			sidebar: [
				{ label: 'Getting Started', slug: 'getting-started' },
				{
					label: 'Guides',
					items: [{ autogenerate: { directory: 'guides' } }],
				},
				{
					label: 'Reference',
					items: [{ autogenerate: { directory: 'reference' } }],
				},
				{
					label: 'Concepts',
					items: [{ autogenerate: { directory: 'concepts' } }],
				},
			],
		}),
	],
});
