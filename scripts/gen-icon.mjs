// Emit a 1024x1024 sage-green PNG with a cream "A" — Anvil's placeholder icon.
// Pure Node (zlib only), no deps, so `npm run icon` works on a clean clone.
import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const S = 1024;
const BG = [94, 138, 90, 255]; // --accent #5e8a5a
const FG = [250, 249, 245, 255]; // --paper #faf9f5

// Crude vector "A": two diagonals + a crossbar, thick strokes.
function isA(x, y) {
  const nx = x / S;
  const ny = y / S;
  if (ny < 0.2 || ny > 0.82) return false;
  const t = (ny - 0.2) / 0.62; // 0 at apex .. 1 at base
  const halfWidth = 0.06 + t * 0.3;
  const leftEdge = 0.5 - halfWidth;
  const rightEdge = 0.5 + halfWidth;
  const stroke = 0.085;
  const onLeft = Math.abs(nx - leftEdge) < stroke;
  const onRight = Math.abs(nx - rightEdge) < stroke;
  const onBar = ny > 0.55 && ny < 0.64 && nx > leftEdge && nx < rightEdge;
  return onLeft || onRight || onBar;
}

const raw = Buffer.alloc(S * (1 + S * 4));
for (let y = 0; y < S; y++) {
  raw[y * (1 + S * 4)] = 0; // filter: none
  for (let x = 0; x < S; x++) {
    const c = isA(x, y) ? FG : BG;
    const o = y * (1 + S * 4) + 1 + x * 4;
    raw[o] = c[0];
    raw[o + 1] = c[1];
    raw[o + 2] = c[2];
    raw[o + 3] = c[3];
  }
}

const crcTable = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();
function crc32(buf) {
  let c = 0xffffffff;
  for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const td = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(td));
  return Buffer.concat([len, td, crc]);
}

const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(S, 0);
ihdr.writeUInt32BE(S, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // RGBA
const png = Buffer.concat([
  sig,
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw)),
  chunk("IEND", Buffer.alloc(0)),
]);
writeFileSync(new URL("./appicon.png", import.meta.url), png);
console.log(`wrote scripts/appicon.png (${png.length} bytes)`);
