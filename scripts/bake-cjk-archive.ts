#!/usr/bin/env bun
/** Bake a density-2 PJFA for Vita streamed CJK (slots 0/7/8). */
const root = process.env.POCKETJS_ROOT || "/root/pocketjs";
const { bakeAtlases } = await import(`${root}/framework/compiler/bake-font.ts`);
const { glyphChecksum } = await import(`${root}/framework/compiler/font-archive.ts`);
const { FONT_ARCHIVE: F } = await import(`${root}/contracts/spec/font-archive.ts`);
const { fontSlotInfo } = await import(`${root}/framework/compiler/tailwind.ts`);
const { createHash } = await import("node:crypto");
const { readFileSync, writeFileSync } = await import("node:fs");

const args = Object.fromEntries(
  Bun.argv.slice(2).map((s) => {
    const at = s.indexOf("=");
    if (at < 3 || !s.startsWith("--"))
      throw new Error("Use --font=FILE --out=FILE --slots=0,7,8 --chars=FILE --density=2");
    return [s.slice(2, at), s.slice(at + 1)];
  }),
);
if (!args.font || !args.out || !args.chars)
  throw new Error("Need --font --out --chars");

const density = Number(args.density ?? "2");
const slots = (args.slots ?? "0,7,8").split(",").map(Number);
const text = readFileSync(args.chars, "utf8");
const codepoints = [...new Set([...text].map((c) => c.codePointAt(0)!).filter(
  (cp) => cp >= 32 && cp !== 127 && !(cp >= 0xd800 && cp <= 0xdfff),
))].sort((a, b) => a - b);
console.log(`codepoints=${codepoints.length} slots=${slots.join(",")} density=${density}`);

const chunks: { descriptor: Uint8Array; index: Uint8Array; cells: Uint8Array }[] = [];
let cursor = F.headerBytes + slots.length * F.strikeBytes;
for (const slot of slots) {
  const [atlas] = await bakeAtlases({
    slots: [slot],
    codepoints,
    regularTtf: args.font,
    boldTtf: args.font,
    monoTtf: args.font,
    fallbackTtfs: [args.font],
    rasterDensity: density,
  });
  const cellW = atlas.coverageW;
  const cellH = atlas.coverageH;
  if (cellW * cellH > F.maxPixels)
    throw new Error(`slot ${slot} coverage ${cellW}x${cellH} exceeds cell budget`);
  const view = new DataView(atlas.bytes.buffer, atlas.bytes.byteOffset, atlas.bytes.byteLength);
  const count = atlas.glyphCount;
  const cell = cellW * cellH;
  const stride = Math.ceil(cell / 4);
  const index = new Uint8Array(count * F.entryBytes);
  const iv = new DataView(index.buffer);
  const cells = new Uint8Array(count * stride);
  const src = 16 + count * 8;
  for (let gid = 0; gid < count; gid++)
    for (let p = 0; p < cell; p++)
      cells[gid * stride + (p >> 2)] |=
        Math.min(3, Math.floor((atlas.bytes[src + gid * cell + p] + 42) / 85)) <<
        (6 - 2 * (p % 4));
  for (let i = 0; i < count; i++) {
    const at = 16 + i * 8;
    const gid = view.getUint16(at + 4, true);
    index.set(atlas.bytes.subarray(at, at + 8), i * 12);
    iv.setUint32(i * 12 + 8, glyphChecksum(cells.subarray(gid * stride, (gid + 1) * stride)), true);
  }
  const descriptor = new Uint8Array(F.strikeBytes);
  const dv = new DataView(descriptor.buffer);
  descriptor.set([
    slot,
    cellW,
    cellH,
    atlas.bytes[10],
    atlas.bytes[11],
    fontSlotInfo(slot).px,
    density,
    atlas.bytes[13],
  ]);
  dv.setUint32(8, count, true);
  dv.setUint32(12, cursor, true);
  cursor += index.length;
  dv.setUint32(16, cursor, true);
  dv.setUint32(20, cells.length, true);
  cursor += cells.length;
  chunks.push({ descriptor, index, cells });
  console.log(`slot ${slot}: ${count} glyphs ${cellW}x${cellH} packed=${cells.length}`);
}
if (cursor > F.maxBytes) throw new Error("Font archive exceeds storage budget");
const out = new Uint8Array(cursor);
const dv = new DataView(out.buffer);
dv.setUint32(0, F.magic, true);
dv.setUint32(4, F.version, true);
dv.setUint32(8, cursor, true);
dv.setUint32(12, slots.length, true);
chunks.forEach((chunk, i) => {
  out.set(chunk.descriptor, F.headerBytes + i * F.strikeBytes);
  const d = new DataView(chunk.descriptor.buffer);
  out.set(chunk.index, d.getUint32(12, true));
  out.set(chunk.cells, d.getUint32(16, true));
});
out.set(createHash("sha256").update(out).digest(), 16);
writeFileSync(args.out, out);
console.log(`${args.out}: ${out.length} bytes`);
