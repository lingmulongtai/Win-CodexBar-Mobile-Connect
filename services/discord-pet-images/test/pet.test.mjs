import { test } from 'node:test';
import assert from 'node:assert/strict';
import sharp from 'sharp';
import { MOTIONS, validSlug, trustedAtlas, convertAtlas } from '../api/pet.js';
test('rejects untrusted slugs and atlas URLs', () => {
  assert.equal(validSlug('maomao-kusuriya'), true);
  for (const slug of ['../foo', 'a/b', 'A', 'x?y', '', 'a'.repeat(101)]) assert.equal(validSlug(slug), false);
  assert.ok(trustedAtlas('cat', { id: 'cat', spritesheetUrl: 'https://codex-pets.net/assets/pets/v/123/cat/spritesheet.webp' }));
  for (const url of ['https://evil.test/assets/pets/cat/spritesheet.webp', 'http://codex-pets.net/assets/pets/cat/spritesheet.webp', 'https://codex-pets.net/assets/pets/other/spritesheet.webp', 'https://codex-pets.net/assets/pets/cat/spritesheet.webp?x=1']) assert.equal(trustedAtlas('cat', { id: 'cat', spritesheetUrl: url }), null);
});
test('all motions encode independent frames with their delays', async () => {
  const w = 1536, h = 1872, raw = Buffer.alloc(w * h * 4);
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const i = (y * w + x) * 4;
    raw[i] = Math.floor(x / 192) * 30 + 10; raw[i + 1] = Math.floor(y / 208) * 25; raw[i + 3] = 255;
  }
  const atlas = await sharp(raw, { raw: { width: w, height: h, channels: 4 } }).webp({ lossless: true }).toBuffer();
  for (const [motion, spec] of Object.entries(MOTIONS)) {
    const gif = await convertAtlas(atlas, motion);
    const meta = await sharp(gif, { animated: true }).metadata();
    assert.equal(meta.pages, spec.delay.length, motion);
    assert.equal(meta.pageHeight, 208, motion);
    assert.deepEqual(meta.delay, spec.delay, motion);
    assert.equal(meta.loop, 0, motion);
  }
  await assert.rejects(() => convertAtlas(atlas, 'unknown'));
  await assert.rejects(() => convertAtlas(Buffer.from('not webp'), 'idle'));
});
