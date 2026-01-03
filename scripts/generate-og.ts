/// <reference types="bun-types" />
// Run with: bun run scripts/generate-og.ts

import satori from "satori";
import { Resvg } from "@resvg/resvg-js";
import { mkdirSync, writeFileSync } from "fs";

// Load font (Inter from Google Fonts CDN)
const fontData = await fetch(
  "https://fonts.gstatic.com/s/inter/v18/UcCO3FwrK3iLTeHuS_nVMrMxCp50SjIw2boKoduKmMEVuLyfAZ9hjp-Ek-_EeA.woff"
).then((res) => res.arrayBuffer());

async function generateImage(
  title: string,
  subtitle: string,
  outputPath: string
): Promise<void> {
  const svg = await satori(
    {
      type: "div",
      props: {
        style: {
          display: "flex",
          flexDirection: "column",
          width: "100%",
          height: "100%",
          background: "#ffffff",
          padding: "60px",
        },
        children: [
          {
            type: "div",
            props: {
              style: {
                fontSize: "72px",
                fontWeight: 600,
                color: "#1d1d1f",
                lineHeight: 1.1,
                letterSpacing: "-0.02em",
              },
              children: title,
            },
          },
          subtitle
            ? {
                type: "div",
                props: {
                  style: {
                    fontSize: "32px",
                    color: "#666",
                    marginTop: "24px",
                  },
                  children: subtitle,
                },
              }
            : null,
          {
            type: "div",
            props: {
              style: {
                marginTop: "auto",
                marginLeft: "auto",
                fontSize: "28px",
                color: "#86868b",
              },
              children: "agentexports.com",
            },
          },
        ].filter(Boolean),
      },
    },
    {
      width: 1200,
      height: 630,
      fonts: [
        {
          name: "Inter",
          data: fontData,
          weight: 600,
          style: "normal",
        },
      ],
    }
  );

  const resvg = new Resvg(svg, {
    fitTo: { mode: "width", value: 1200 },
  });
  const png = resvg.render().asPng();

  writeFileSync(outputPath, png);
  console.log(`Generated: ${outputPath}`);
}

// Ensure output directory exists
mkdirSync("./worker/static", { recursive: true });

// Generate homepage OG image
await generateImage(
  "agentexport",
  "Share Claude Code and Codex transcripts",
  "./worker/static/og-homepage.png"
);

// Generate generic viewer OG image
await generateImage(
  "Shared Transcript",
  "View a Claude Code or Codex session",
  "./worker/static/og-viewer.png"
);

console.log("\nDone! Images saved to ./worker/static/");
