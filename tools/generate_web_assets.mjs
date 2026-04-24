#!/usr/bin/env node

import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { Resvg } from "@resvg/resvg-js";
import gifenc from "gifenc";
import { PNG } from "pngjs";

const { GIFEncoder, quantize, applyPalette } = gifenc;

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.resolve(__dirname, "..");

const HERO_OUT = path.join(ROOT, "docs/assets/hero/shac-hero.gif");
const HERO_POSTER_OUT = path.join(ROOT, "docs/assets/hero/shac-hero-poster.png");
const OG_OUT = path.join(ROOT, "docs/assets/social/shac-og.png");

const c = {
  bg: "#09110f",
  bgSoft: "#12221f",
  ink: "#eef8f3",
  muted: "#9eb8ae",
  accent: "#78ffbb",
  accent2: "#4de4ff",
  card: "#12201e",
  cardStrong: "#081210",
  shell: "#0c1815",
};

const scenes = [
  {
    label: "Finish commands faster.",
    prompt: "git ch",
    cursor: 6,
    suggestions: [
      "checkout       switch branches or restore working tree files",
      "cherry-pick    apply commits from another branch",
      "check-ignore   debug ignore rules",
    ],
    active: 0,
  },
  {
    label: "Rank with context + docs.",
    prompt: "kubectl con",
    cursor: 11,
    suggestions: [
      "config current-context   show active kube context",
      "config set-context       create or modify kube context",
      "config get-contexts      list configured contexts",
    ],
    active: 0,
  },
  {
    label: "Stay local. Stay in flow.",
    prompt: "pyt",
    cursor: 3,
    suggestions: [
      "python3        run the Python 3 interpreter",
      "pytest         run Python tests",
      "python -m venv create a virtual environment",
    ],
    active: 0,
  },
];

function escapeXml(value) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;");
}

function sceneSvg(frame) {
  const scene = scenes[Math.floor(frame / 6) % scenes.length];
  const sceneFrame = frame % 6;
  const typed = scene.prompt.slice(0, Math.max(1, Math.min(scene.prompt.length, sceneFrame + 1)));
  const showMenu = sceneFrame >= 2;
  const cursorX = 116 + typed.length * 14;

  const suggestionLines = showMenu
    ? scene.suggestions
        .map((line, index) => {
          const active = index === scene.active;
          const y = 252 + index * 44;
          return `
            <rect x="114" y="${y - 26}" width="690" height="34" rx="12" fill="${active ? "rgba(120,255,187,0.16)" : "rgba(255,255,255,0.03)"}" />
            <text x="134" y="${y}" fill="${active ? c.accent : c.ink}" font-size="18" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">${escapeXml(line)}</text>
          `;
        })
        .join("")
    : "";

  return `
    <svg xmlns="http://www.w3.org/2000/svg" width="1100" height="620" viewBox="0 0 1100 620">
      <defs>
        <filter id="blur-xl"><feGaussianBlur stdDeviation="56" /></filter>
        <filter id="blur-lg"><feGaussianBlur stdDeviation="28" /></filter>
      </defs>
      <rect width="1100" height="620" fill="${c.bg}" />
      <circle cx="190" cy="70" r="140" fill="${c.accent}" fill-opacity="0.22" filter="url(#blur-xl)" />
      <circle cx="980" cy="560" r="180" fill="${c.accent2}" fill-opacity="0.14" filter="url(#blur-xl)" />
      <circle cx="880" cy="140" r="120" fill="${c.accent}" fill-opacity="0.08" filter="url(#blur-lg)" />

      <text x="72" y="84" fill="${c.ink}" font-size="52" font-weight="700" font-family="'DejaVu Sans', sans-serif">Smart local shell autocomplete.</text>
      <text x="72" y="126" fill="${c.muted}" font-size="22" font-family="'DejaVu Sans', sans-serif">${escapeXml(scene.label)}</text>

      <rect x="72" y="166" width="956" height="368" rx="28" fill="${c.card}" stroke="rgba(120,255,187,0.14)" />
      <rect x="72" y="166" width="956" height="42" rx="28" fill="${c.cardStrong}" />
      <circle cx="102" cy="187" r="6" fill="#ff7a90" />
      <circle cx="122" cy="187" r="6" fill="#ffd66d" />
      <circle cx="142" cy="187" r="6" fill="#6ef7c1" />
      <text x="172" y="193" fill="${c.muted}" font-size="15" font-family="'DejaVu Sans', sans-serif">zsh · shac daemon running · local only</text>

      <text x="110" y="244" fill="${c.accent2}" font-size="22" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">❯</text>
      <text x="138" y="244" fill="${c.ink}" font-size="22" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">${escapeXml(typed)}</text>
      <rect x="${cursorX}" y="226" width="10" height="24" rx="3" fill="${sceneFrame % 2 === 0 ? c.accent : "rgba(120,255,187,0.3)"}" />

      ${suggestionLines}

      <rect x="72" y="560" width="320" height="34" rx="17" fill="rgba(120,255,187,0.12)" />
      <text x="94" y="582" fill="${c.accent}" font-size="16" font-weight="700" font-family="'DejaVu Sans', sans-serif">zsh + bash · context-aware ranking · local privacy</text>
    </svg>
  `;
}

function ogSvg() {
  return `
    <svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630">
      <defs>
        <filter id="blur-xl"><feGaussianBlur stdDeviation="58" /></filter>
      </defs>
      <rect width="1200" height="630" fill="${c.bg}" />
      <circle cx="170" cy="92" r="180" fill="${c.accent}" fill-opacity="0.20" filter="url(#blur-xl)" />
      <circle cx="1080" cy="530" r="220" fill="${c.accent2}" fill-opacity="0.15" filter="url(#blur-xl)" />

      <rect x="74" y="64" width="92" height="92" rx="24" fill="${c.accent}" />
      <path d="M106 110l18-18v12h38v12h-38v12z" fill="#05100d" />
      <rect x="144" y="126" width="18" height="8" rx="4" fill="#05100d" />
      <text x="194" y="120" fill="${c.ink}" font-size="46" font-weight="700" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">shac</text>

      <text x="76" y="226" fill="${c.ink}" font-size="66" font-weight="700" font-family="'DejaVu Sans', sans-serif">Smart local shell</text>
      <text x="76" y="298" fill="${c.ink}" font-size="66" font-weight="700" font-family="'DejaVu Sans', sans-serif">autocomplete</text>
      <text x="76" y="354" fill="${c.muted}" font-size="26" font-family="'DejaVu Sans', sans-serif">Fewer keystrokes, context-aware ranking, no network path.</text>

      <rect x="76" y="402" width="250" height="48" rx="24" fill="rgba(120,255,187,0.12)" />
      <text x="201" y="432" text-anchor="middle" fill="${c.accent}" font-size="18" font-weight="700" font-family="'DejaVu Sans', sans-serif">zsh + bash · local-first</text>

      <rect x="650" y="86" width="472" height="446" rx="28" fill="${c.card}" stroke="rgba(120,255,187,0.18)" />
      <text x="682" y="130" fill="${c.muted}" font-size="16" font-family="'DejaVu Sans', sans-serif">shell session</text>
      <text x="682" y="198" fill="${c.accent2}" font-size="24" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">❯</text>
      <text x="712" y="198" fill="${c.ink}" font-size="24" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">git ch</text>
      <rect x="682" y="236" width="406" height="38" rx="14" fill="rgba(120,255,187,0.16)" />
      <text x="702" y="261" fill="${c.accent}" font-size="19" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">checkout       switch branches</text>
      <text x="702" y="310" fill="${c.ink}" font-size="19" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">cherry-pick    apply commits</text>
      <text x="702" y="359" fill="${c.ink}" font-size="19" font-family="'IBM Plex Mono', 'DejaVu Sans Mono', monospace">check-ignore   debug ignore</text>
      <text x="682" y="470" fill="${c.muted}" font-size="24" font-family="'DejaVu Sans', sans-serif">github.com/Neftedollar/sh-autocomplete</text>
    </svg>
  `;
}

function renderSvg(svg, width) {
  const resvg = new Resvg(svg, {
    fitTo: { mode: "width", value: width },
    background: "rgba(255,255,255,0)",
    font: {
      loadSystemFonts: true,
      defaultFontFamily: "DejaVu Sans",
      defaultMonospaceFontFamily: "DejaVu Sans Mono",
    },
  });
  return resvg.render().asPng();
}

async function write(filePath, buffer) {
  await fs.mkdir(path.dirname(filePath), { recursive: true });
  await fs.writeFile(filePath, buffer);
}

async function generateHero() {
  const width = 1100;
  const height = 620;
  const frames = [];
  for (let frame = 0; frame < 18; frame += 1) {
    frames.push(renderSvg(sceneSvg(frame), width));
  }
  await write(HERO_POSTER_OUT, frames.at(-1));

  const gif = GIFEncoder();
  for (let frame = 0; frame < frames.length; frame += 1) {
    const { data } = PNG.sync.read(frames[frame]);
    const palette = quantize(data, 256, { format: "rgba4444" });
    const indexed = applyPalette(data, palette, "rgba4444");
    gif.writeFrame(indexed, width, height, {
      palette,
      delay: frame % 6 === 5 ? 220 : 110,
      repeat: 0,
    });
  }
  gif.finish();
  await write(HERO_OUT, Buffer.from(gif.bytes()));
}

async function generateOg() {
  await write(OG_OUT, renderSvg(ogSvg(), 1200));
}

async function main() {
  await generateHero();
  await generateOg();
  console.log(`wrote ${path.relative(ROOT, HERO_OUT)}`);
  console.log(`wrote ${path.relative(ROOT, HERO_POSTER_OUT)}`);
  console.log(`wrote ${path.relative(ROOT, OG_OUT)}`);
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
