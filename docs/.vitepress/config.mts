import { defineConfig } from "vitepress";
import { readFileSync } from "node:fs";

// The app version, straight from the crate manifest, for the nav badge.
const version = readFileSync(new URL("../../Cargo.toml", import.meta.url), "utf8").match(/^version = "(.+)"/m)?.[1] ?? "";
const repo = "https://github.com/irooftop/rooftop";

export default defineConfig({
  title: "iRoof.top",
  description: "Continue a Telegram or WhatsApp chat with an LLM that writes in the style of a person you miss.",
  lang: "en-US",
  base: "/rooftop/",
  cleanUrls: true,
  lastUpdated: true,
  appearance: true,
  // Quiet, nearly monochrome code, like the rest of the page.
  markdown: { theme: { light: "min-light", dark: "min-dark" } },
  head: [
    ["link", { rel: "icon", type: "image/svg+xml", href: "/rooftop/img/favicon.svg" }],
    ["meta", { name: "theme-color", content: "#ffffff", media: "(prefers-color-scheme: light)" }],
    ["meta", { name: "theme-color", content: "#0b1016", media: "(prefers-color-scheme: dark)" }],
    ["meta", { property: "og:title", content: "iRoof.top" }],
    ["meta", { property: "og:description", content: "Talk to the people you miss: an LLM continues your Telegram or WhatsApp chat in their style." }],
  ],
  themeConfig: {
    logo: { light: "/img/icon_light.svg", dark: "/img/icon_dark.svg", alt: "" },
    siteTitle: "iRoof.top",
    nav: [
      { text: "Guide", link: "/guide/getting-started", activeMatch: "/guide/" },
      { text: "Features", link: "/features/how-it-works", activeMatch: "/features/" },
      { text: "Reference", link: "/reference/configuration", activeMatch: "/reference/" },
      {
        text: `v${version}`,
        items: [
          { text: "Releases", link: `${repo}/releases` },
          { text: "Changelog", link: `${repo}/releases/tag/v${version}` },
        ],
      },
    ],
    sidebar: [
      {
        text: "Guide",
        items: [
          { text: "Getting started", link: "/guide/getting-started" },
          { text: "Offline & privacy", link: "/guide/offline" },
          { text: "Export a chat", link: "/guide/export" },
          { text: "Desktop app", link: "/guide/desktop" },
          { text: "Open an unsigned app", link: "/guide/unsigned-apps" },
          { text: "Docker & servers", link: "/guide/docker" },
        ],
      },
      {
        text: "Features",
        items: [
          { text: "How it works", link: "/features/how-it-works" },
          { text: "Share links", link: "/features/share-links" },
          { text: "Answer from Telegram", link: "/features/telegram" },
          { text: "Photos & voice", link: "/features/media" },
          { text: "MCP tools", link: "/features/mcp" },
        ],
      },
      {
        text: "Reference",
        items: [
          { text: "Configuration", link: "/reference/configuration" },
          { text: "CLI", link: "/reference/cli" },
          { text: "HTTP API", link: "/reference/http-api" },
          { text: "Relay hub", link: "/reference/relay-hub" },
          { text: "Development", link: "/reference/development" },
        ],
      },
    ],
    socialLinks: [{ icon: "github", link: repo }],
    editLink: { pattern: `${repo}/edit/main/docs/:path`, text: "Edit this page on GitHub" },
    search: { provider: "local" },
    outline: { level: [2, 3] },
    footer: { message: "Released under the MIT License.", copyright: "Made for staying close to people who are far away." },
  },
});
