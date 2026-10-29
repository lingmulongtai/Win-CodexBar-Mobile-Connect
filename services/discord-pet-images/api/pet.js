import sharp from "sharp";

export const MOTIONS = Object.freeze({
  idle: { row: 0, delay: [280, 110, 110, 140, 140, 320] },
  "running-right": { row: 1, delay: [120, 120, 120, 120, 120, 120, 120, 220] },
  "running-left": { row: 2, delay: [120, 120, 120, 120, 120, 120, 120, 220] },
  waving: { row: 3, delay: [140, 140, 140, 280] },
  jumping: { row: 4, delay: [140, 140, 140, 140, 280] },
  failed: { row: 5, delay: [140, 140, 140, 140, 140, 140, 140, 240] },
  waiting: { row: 6, delay: [150, 150, 150, 150, 150, 260] },
  running: { row: 7, delay: [120, 120, 120, 120, 120, 220] },
  review: { row: 8, delay: [150, 150, 150, 150, 150, 280] },
});
const MAX_ATLAS = 4 * 1024 * 1024;
const MAX_RESPONSE = 4 * 1024 * 1024;
const CELL_WIDTH = 192;
const CELL_HEIGHT = 208;
let active = 0;

export function validSlug(value) {
  return typeof value === "string" && value.length <= 100 && /^[a-z0-9]+(?:-[a-z0-9]+)*$/.test(value);
}

export function trustedAtlas(slug, pet) {
  if (!validSlug(slug) || pet?.id !== slug || typeof pet.spritesheetUrl !== "string") return null;
  let url;
  try { url = new URL(pet.spritesheetUrl); } catch { return null; }
  const expected = new RegExp(`^/assets/pets/(?:v/[0-9]+/)?${slug}/spritesheet\\.webp$`);
  if (url.protocol !== "https:" || url.hostname !== "codex-pets.net" || url.port || url.username || url.password || url.search || url.hash || !expected.test(url.pathname)) return null;
  return url.href;
}

async function boundedFetch(url, limit) {
  const res = await fetch(url, { redirect: "error", signal: AbortSignal.timeout(10_000), headers: { Accept: "application/json,image/webp" } });
  if (!res.ok || !res.body) throw new Error("source unavailable");
  const declared = Number(res.headers.get("content-length"));
  if (Number.isFinite(declared) && declared > limit) throw new Error("source too large");
  const chunks = [];
  let total = 0;
  for await (const chunk of res.body) {
    total += chunk.length;
    if (total > limit) throw new Error("source too large");
    chunks.push(chunk);
  }
  return Buffer.concat(chunks, total);
}

export async function convertAtlas(atlas, motion) {
  const spec = MOTIONS[motion];
  if (!spec) throw new Error("invalid motion");
  const metadata = await sharp(atlas, { limitInputPixels: 4_000_000 }).metadata();
  if (metadata.format !== "webp" || metadata.width !== 1536 || ![1872, 2288].includes(metadata.height)) throw new Error("unsupported atlas");
  const frames = [];
  for (let col = 0; col < spec.delay.length; col++) {
    frames.push(await sharp(atlas, { limitInputPixels: 4_000_000 })
      .extract({ left: col * CELL_WIDTH, top: spec.row * CELL_HEIGHT, width: CELL_WIDTH, height: CELL_HEIGHT })
      .ensureAlpha().raw().toBuffer());
  }
  const image = await sharp(Buffer.concat(frames), { raw: { width: CELL_WIDTH, height: CELL_HEIGHT * frames.length, channels: 4, pageHeight: CELL_HEIGHT } })
    .gif({ loop: 0, delay: spec.delay, effort: 4 }).toBuffer();
  if (image.length > MAX_RESPONSE) throw new Error("output too large");
  return image;
}

export default async function handler(req, res) {
  if (req.method !== "GET") { res.setHeader("Allow", "GET"); return res.status(405).end(); }
  const url = new URL(req.url, "https://example.invalid");
  const slug = url.searchParams.get("pet");
  const motion = url.searchParams.get("motion");
  if (!validSlug(slug) || !Object.hasOwn(MOTIONS, motion)) return res.status(400).send("Invalid pet or motion");
  if (active >= 4) { res.setHeader("Retry-After", "5"); return res.status(503).send("Busy"); }
  active++;
  try {
    const data = JSON.parse((await boundedFetch(`https://codex-pets.net/api/pets/${slug}`, 64 * 1024)).toString("utf8"));
    const atlasUrl = trustedAtlas(slug, data.pet);
    if (!atlasUrl) return res.status(404).send("Pet unavailable");
    const atlas = await boundedFetch(atlasUrl, MAX_ATLAS);
    const gif = await convertAtlas(atlas, motion);
    res.setHeader("Content-Type", "image/gif");
    res.setHeader("Cache-Control", "public, max-age=3600, s-maxage=3600");
    res.setHeader("X-Content-Type-Options", "nosniff");
    return res.status(200).send(gif);
  } catch {
    res.setHeader("Cache-Control", "no-store");
    return res.status(502).send("Pet image unavailable");
  } finally { active--; }
}
