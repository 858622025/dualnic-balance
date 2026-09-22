// build_icon.mjs —— 由 assets/icon.svg 生成各尺寸 PNG 与 icon.ico
// 用法（依赖临时装在 target/，不进 git）：
//   cd dualnic-balance/target/icon-tools && bun add @resvg/resvg-js png-to-ico
//   bun run ../../assets/build_icon.mjs
import { createRequire } from "node:module";
import { readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

// 从运行目录（target/icon-tools）解析依赖
const require = createRequire(path.join(process.cwd(), "node_modules", "_"));
const { Resvg } = require("@resvg/resvg-js");
const pngToIcoMod = require("png-to-ico"); // v3 为 ESM，兼容取 default
const pngToIco = pngToIcoMod.default ?? pngToIcoMod;

const svg = readFileSync(new URL("./icon.svg", import.meta.url), "utf8");
const sizes = [256, 128, 64, 48, 32, 16];

const pngs = sizes.map((s) => {
  const buf = new Resvg(svg, { fitTo: { mode: "width", value: s } }).render().asPng();
  writeFileSync(new URL(`./icon-${s}.png`, import.meta.url), buf);
  return buf;
});

writeFileSync(new URL("./icon.ico", import.meta.url), await pngToIco(pngs));
console.log("done:", sizes.join(", "));
