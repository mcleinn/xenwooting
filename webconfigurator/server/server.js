import express from 'express'
import path from 'node:path'
import fs from 'node:fs/promises'
import fsSync from 'node:fs'
import TOML from '@iarna/toml'

import { formatWtn, readWtnFile, writeWtnFile } from './wtn.js'
import { loadPlayableGeometry } from './geometry.js'
import { buildChordCatalogue, findChordNames, loadScalaChordNamesDb } from './chords.js'
import { computeSharedIsomorphicAxisSteps } from './isomorphic.js'

const APP_BASE = '/wtn'
const API_BASE = `${APP_BASE}/api`

const CONFIG_DIR = process.env.XENWTN_CONFIG_DIR || '/home/patch/.config/xenwooting'
const CONFIG_TOML = process.env.XENWTN_CONFIG_TOML || path.join(CONFIG_DIR, 'config.toml')
const GEOMETRY_JSON = process.env.XENWTN_GEOMETRY_JSON || '/home/patch/xenWTN.json'
const PORT = Number.parseInt(process.env.PORT || '3174', 10)

const DIST_DIR = path.resolve(new URL('../web/dist', import.meta.url).pathname)

const app = express()
app.use(express.json({ limit: '2mb' }))

async function reloadXenwooting() {
  // xenwooting watches the .wtn file and reloads on change.
  return { ok: true }
}

function defaultOutputDir() {
  const xdg = String(process.env.XDG_STATE_HOME || '').trim()
  if (xdg) return path.join(xdg, 'xenwooting')
  const home = String(process.env.HOME || '').trim()
  if (home) return path.join(home, '.local', 'state', 'xenwooting')
  return '/tmp/xenwooting'
}

async function resolveOutputDir() {
  try {
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const od = String(cfg?.output_dir || '').trim()
    if (od) return od
  } catch {
    // ignore
  }
  return defaultOutputDir()
}

async function resolveBoards() {
  try {
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const boards = Array.isArray(cfg?.boards) ? cfg.boards : []
    const out = []
    for (const b of boards) {
      const deviceId = String(b?.device_id || '').trim()
      const wtnBoard = Number(b?.wtn_board)
      if (!deviceId) continue
      if (!Number.isFinite(wtnBoard)) continue
      out.push({ deviceId, wtnBoard })
    }
    return out
  } catch {
    return []
  }
}

function parseDumpFilename(name) {
  const m = String(name || '').match(/^(\d+)_(dump|capture)\.(csv|txt)$/)
  if (!m) return null
  return { tsMs: Number.parseInt(m[1], 10), kind: m[2], ext: m[3] }
}

const PREVIEW_ENABLED_PATH = process.env.XENWTN_PREVIEW_ENABLED_PATH || '/tmp/xenwooting-preview.enabled'
const PREVIEW_WTN_PATH = process.env.XENWTN_PREVIEW_WTN_PATH || '/tmp/xenwooting-preview.wtn'
const TRAINER_MODE_PATH = process.env.XENWTN_TRAINER_MODE_PATH || '/tmp/xenwooting-trainer.mode'
const GUIDE_PATH = process.env.XENWTN_GUIDE_PATH || '/tmp/xenwooting-guide.json'
const HIGHLIGHT_PATH = process.env.XENWTN_HIGHLIGHT_PATH || '/tmp/xenwooting-highlight.txt'

const LIVE_STATE_PATH = process.env.XENWTN_LIVE_STATE_PATH || '/tmp/xenwooting-live.json'

const XENHARM_URL = process.env.XENHARM_URL || 'http://127.0.0.1:3199'

const SCALA_CHORD_DB_PATH = path.resolve(new URL('./data/scala/chordnam.par', import.meta.url).pathname)
let SCALA_CHORD_DB = null
;(async () => {
  try {
    SCALA_CHORD_DB = await loadScalaChordNamesDb(SCALA_CHORD_DB_PATH)
    // eslint-disable-next-line no-console
    console.log(`Loaded Scala chord names: ${SCALA_CHORD_DB_PATH}`)
  } catch (e) {
    // eslint-disable-next-line no-console
    console.warn(`Failed to load chord names db (${SCALA_CHORD_DB_PATH}): ${String(e?.message || e)}`)
    SCALA_CHORD_DB = null
  }
})()

// Best-effort: precompute approximation map for the currently active EDO.
// This avoids extra work during play when chord names are requested.
let LIVE_LAST_EDO = null
setInterval(async () => {
  if (!SCALA_CHORD_DB) return
  try {
    const raw = await fs.readFile(LIVE_STATE_PATH, 'utf8')
    const obj = JSON.parse(raw)
    const edo = Number.parseInt(String(obj?.layout?.edo ?? ''), 10)
    if (!Number.isInteger(edo) || edo < 1 || edo > 999) return
    if (LIVE_LAST_EDO === edo) return
    LIVE_LAST_EDO = edo
    SCALA_CHORD_DB.ensureForEdo?.(edo)
  } catch {
    // ignore
  }
}, 1000)

// Simple in-memory cache for note names.
// key: `${edo}:${pitch}` -> { short, unicode, alts?: [{short, unicode}] } | null
const NOTE_NAME_CACHE = new Map()

async function writeFileAtomic(filePath, text) {
  const tmp = `${filePath}.tmp-${process.pid}-${Date.now()}`
  await fs.writeFile(tmp, text, 'utf8')
  await fs.rename(tmp, filePath)
}

async function safeUnlink(filePath) {
  try {
    await fs.unlink(filePath)
  } catch (e) {
    if (e && typeof e === 'object' && 'code' in e && e.code === 'ENOENT') return
    throw e
  }
}

function boardNameToIndex(name) {
  return name === 'Board0' ? 0 : name === 'Board1' ? 1 : null
}

app.get(`${API_BASE}/layouts`, async (_req, res) => {
  try {
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const layouts = Array.isArray(cfg.layouts) ? cfg.layouts : []

    const outLayouts = layouts
      .map((l) => ({
        id: String(l.id || ''),
        name: String(l.name || l.id || ''),
        wtnPath: String(l.wtn_path || ''),
      }))
      .filter((l) => l.id && l.wtnPath)
      .sort((a, b) => naturalCompare(a.name || a.id, b.name || b.id) || naturalCompare(a.id, b.id))

    res.json({
      configDir: CONFIG_DIR,
      layouts: outLayouts,
    })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.get(`${API_BASE}/dumps/list`, async (_req, res) => {
  try {
    const outDir = await resolveOutputDir()
    const boards = await resolveBoards()
    let names = []
    try {
      names = await fs.readdir(outDir)
    } catch {
      res.json({ outputDir: outDir, boards, entries: [] })
      return
    }

    const byKey = new Map()
    for (const name of names) {
      const p = parseDumpFilename(name)
      if (!p || !Number.isFinite(p.tsMs)) continue
      const fp = path.join(outDir, name)
      let st = null
      try {
        st = await fs.stat(fp)
      } catch {
        continue
      }
      const k = `${p.tsMs}:${p.kind}`
      const cur = byKey.get(k) || {
        tsMs: p.tsMs,
        kind: p.kind,
        hasCsv: false,
        hasTxt: false,
        csvName: null,
        txtName: null,
        sizeCsv: 0,
        sizeTxt: 0,
        mtimeMs: 0,
      }
      cur.mtimeMs = Math.max(cur.mtimeMs, Number(st.mtimeMs || 0))
      if (p.ext === 'csv') {
        cur.hasCsv = true
        cur.csvName = name
        cur.sizeCsv = Number(st.size || 0)
      } else if (p.ext === 'txt') {
        cur.hasTxt = true
        cur.txtName = name
        cur.sizeTxt = Number(st.size || 0)
      }
      byKey.set(k, cur)
    }

    const entries = Array.from(byKey.values()).sort((a, b) => (b.tsMs - a.tsMs) || String(a.kind).localeCompare(String(b.kind)))
    res.json({ outputDir: outDir, boards, entries })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.get(`${API_BASE}/dumps/:ts/:kind.:ext`, async (req, res) => {
  const ts = String(req.params.ts || '')
  const kind = String(req.params.kind || '')
  const ext = String(req.params.ext || '')
  if (!/^\d+$/.test(ts)) {
    res.status(400).send('Bad ts')
    return
  }
  if (kind !== 'dump' && kind !== 'capture') {
    res.status(400).send('Bad kind')
    return
  }
  if (ext !== 'csv' && ext !== 'txt') {
    res.status(400).send('Bad ext')
    return
  }

  const outDir = await resolveOutputDir()
  const name = `${ts}_${kind}.${ext}`
  const fp = path.join(outDir, name)
  let st = null
  try {
    st = await fs.stat(fp)
    if (!st.isFile()) throw new Error('not file')
  } catch {
    res.status(404).send('Not found')
    return
  }

  const total = Number(st.size || 0)
  const range = String(req.headers.range || '')
  let start = 0
  let end = Math.max(0, total - 1)
  let partial = false

  if (range) {
    const m = range.match(/^bytes=(\d*)-(\d*)$/)
    if (!m) {
      res.status(416).setHeader('Content-Range', `bytes */${total}`).end()
      return
    }
    const a = m[1]
    const b = m[2]
    if (a === '' && b === '') {
      res.status(416).setHeader('Content-Range', `bytes */${total}`).end()
      return
    }

    if (a === '') {
      // suffix: last N bytes
      const n = Number.parseInt(b, 10)
      if (!Number.isFinite(n) || n <= 0) {
        res.status(416).setHeader('Content-Range', `bytes */${total}`).end()
        return
      }
      start = Math.max(0, total - n)
      end = Math.max(0, total - 1)
    } else {
      start = Number.parseInt(a, 10)
      end = b === '' ? Math.max(0, total - 1) : Number.parseInt(b, 10)
      if (!Number.isFinite(start) || !Number.isFinite(end) || start < 0 || end < start) {
        res.status(416).setHeader('Content-Range', `bytes */${total}`).end()
        return
      }
      if (start >= total) {
        res.status(416).setHeader('Content-Range', `bytes */${total}`).end()
        return
      }
      end = Math.min(end, Math.max(0, total - 1))
    }
    partial = true
  }

  const contentLen = Math.max(0, end - start + 1)
  res.setHeader('Accept-Ranges', 'bytes')
  if (partial) {
    res.status(206)
    res.setHeader('Content-Range', `bytes ${start}-${end}/${total}`)
    res.setHeader('Content-Length', String(contentLen))
  } else {
    res.setHeader('Content-Length', String(total))
  }

  res.setHeader('Cache-Control', 'no-store')
  res.setHeader('Content-Type', ext === 'csv' ? 'text/csv; charset=utf-8' : 'text/plain; charset=utf-8')

  const stream = fsSync.createReadStream(fp, { start, end: partial ? end : undefined })
  stream.on('error', () => {
    res.status(500).end('Read error')
  })
  stream.pipe(res)
})

function naturalCompare(a, b) {
  const ax = String(a ?? '')
  const bx = String(b ?? '')
  if (ax === bx) return 0

  const as = ax.split(/(\d+)/).filter(Boolean)
  const bs = bx.split(/(\d+)/).filter(Boolean)
  const n = Math.max(as.length, bs.length)
  for (let i = 0; i < n; i++) {
    const ap = as[i]
    const bp = bs[i]
    if (ap === undefined) return -1
    if (bp === undefined) return 1
    const an = /^\d+$/.test(ap) ? Number.parseInt(ap, 10) : null
    const bn = /^\d+$/.test(bp) ? Number.parseInt(bp, 10) : null
    if (an !== null && bn !== null) {
      if (an !== bn) return an - bn
      continue
    }
    const c = ap.localeCompare(bp, undefined, { sensitivity: 'base' })
    if (c !== 0) return c
  }
  return ax.localeCompare(bx, undefined, { sensitivity: 'base' })
}

app.post(`${API_BASE}/layouts/add`, async (req, res) => {
  try {
    const name = String(req.body?.name || '').trim()
    const edoDivisions = Number.parseInt(String(req.body?.edoDivisions ?? ''), 10)
    const pitchOffset = Number.parseInt(String(req.body?.pitchOffset ?? 0), 10)
    if (!name) {
      res.status(400).json({ error: 'Expected body: { name }' })
      return
    }
    if (!Number.isFinite(edoDivisions) || edoDivisions < 1 || edoDivisions > 999) {
      res.status(400).json({ error: 'Expected body: { edoDivisions: int >= 1 }' })
      return
    }
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const layouts = Array.isArray(cfg.layouts) ? cfg.layouts : []

    const baseId = `edo${edoDivisions}`
    const used = new Set(layouts.map((l) => String(l.id || '')))
    let id = baseId
    if (used.has(id)) {
      let n = 2
      while (used.has(`${baseId}-${n}`)) n++
      id = `${baseId}-${n}`
    }

    const wtnRel = `wtn/${id}.wtn`
    const wtnAbs = path.join(CONFIG_DIR, wtnRel)

    // Create minimal .wtn file if missing.
    try {
      await fs.mkdir(path.dirname(wtnAbs), { recursive: true })
      await fs.access(wtnAbs)
    } catch {
      await writeFileAtomic(wtnAbs, `[Board0]\n\n[Board1]\n\n`)
    }

    const block =
      `\n[[layouts]]\n` +
      `id = ${JSON.stringify(id)}\n` +
      `name = ${JSON.stringify(name)}\n` +
      `wtn_path = ${JSON.stringify(wtnRel)}\n` +
      `edo_divisions = ${edoDivisions}\n` +
      `pitch_offset = ${Number.isFinite(pitchOffset) ? pitchOffset : 0}\n`

    await writeFileAtomic(CONFIG_TOML, `${raw.trimEnd()}${block}`)

    res.json({ id, name })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.get(`${API_BASE}/layout/:id`, async (req, res) => {
  try {
    const id = String(req.params.id)
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const layout = (Array.isArray(cfg.layouts) ? cfg.layouts : []).find((l) => String(l.id) === id)
    if (!layout) {
      res.status(404).json({ error: `Unknown layout id: ${id}` })
      return
    }

    const wtnPathRel = String(layout.wtn_path || '')
    const wtnPathAbs = path.join(CONFIG_DIR, wtnPathRel)
    const boards = await readWtnFile(wtnPathAbs)

    res.json({
      id,
      name: String(layout.name || id),
      wtnPath: wtnPathRel,
      wtnPathAbs,
      edoDivisions: Number(layout.edo_divisions ?? 12),
      pitchOffset: Number(layout.pitch_offset ?? 0),
      boards,
    })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.get(`${API_BASE}/layout/:id/isomorphic`, async (req, res) => {
  try {
    const id = String(req.params.id)
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const layout = (Array.isArray(cfg.layouts) ? cfg.layouts : []).find((l) => String(l.id) === id)
    if (!layout) {
      res.status(404).json({ error: `Unknown layout id: ${id}` })
      return
    }

    const wtnPathRel = String(layout.wtn_path || '')
    const wtnPathAbs = path.join(CONFIG_DIR, wtnPathRel)
    const boards = await readWtnFile(wtnPathAbs)
    const edo = Number(layout.edo_divisions ?? 12)
    const pitchOffset = Number(layout.pitch_offset ?? 0)

    const geom = await loadPlayableGeometry(GEOMETRY_JSON)
    const iso = computeSharedIsomorphicAxisSteps({ geometryKeys: geom, boards, edo, pitchOffset })
    res.json({ layoutId: id, ok: Boolean(iso?.ok), edo: iso?.edo ?? edo, dq: iso?.dq ?? null, dr: iso?.dr ?? null, axis3: iso?.axis3 ?? null, reason: iso?.reason || null })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.post(`${API_BASE}/layout/:id/settings`, async (req, res) => {
  try {
    const id = String(req.params.id)
    const name = String(req.body?.name || '').trim()
    const edoDivisions = Number.parseInt(String(req.body?.edoDivisions ?? ''), 10)
    if (!name) {
      res.status(400).json({ error: 'Expected body: { name }' })
      return
    }
    if (!Number.isFinite(edoDivisions) || edoDivisions < 1 || edoDivisions > 999) {
      res.status(400).json({ error: 'Expected body: { edoDivisions: int >= 1 }' })
      return
    }

    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const layouts = Array.isArray(cfg.layouts) ? cfg.layouts : []
    const idx = layouts.findIndex((l) => String(l.id) === id)
    if (idx < 0) {
      res.status(404).json({ error: `Unknown layout id: ${id}` })
      return
    }

    layouts[idx] = {
      ...layouts[idx],
      id,
      name,
      edo_divisions: edoDivisions,
    }
    cfg.layouts = layouts

    const out = TOML.stringify(cfg)
    await writeFileAtomic(CONFIG_TOML, out)
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.delete(`${API_BASE}/layout/:id`, async (req, res) => {
  try {
    const id = String(req.params.id)
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const layouts = Array.isArray(cfg.layouts) ? cfg.layouts : []
    if (layouts.length <= 1) {
      res.status(400).json({ error: 'Cannot delete the last remaining layout' })
      return
    }
    const idx = layouts.findIndex((l) => String(l.id) === id)
    if (idx < 0) {
      res.status(404).json({ error: `Unknown layout id: ${id}` })
      return
    }

    const wtnPathRel = String(layouts[idx].wtn_path || '')
    layouts.splice(idx, 1)
    cfg.layouts = layouts

    const out = TOML.stringify(cfg)
    await writeFileAtomic(CONFIG_TOML, out)

    if (wtnPathRel) {
      const wtnAbs = path.join(CONFIG_DIR, wtnPathRel)
      await safeUnlink(wtnAbs)
    }

    const nextId = String(layouts[0]?.id || '')
    res.json({ ok: true, nextId })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.post(`${API_BASE}/layout/:id`, async (req, res) => {
  try {
    const id = String(req.params.id)
    const raw = await fs.readFile(CONFIG_TOML, 'utf8')
    const cfg = TOML.parse(raw)
    const layout = (Array.isArray(cfg.layouts) ? cfg.layouts : []).find((l) => String(l.id) === id)
    if (!layout) {
      res.status(404).json({ error: `Unknown layout id: ${id}` })
      return
    }

    const boards = req.body?.boards
    if (!boards || !boards.Board0 || !boards.Board1) {
      res.status(400).json({ error: 'Expected body: { boards: { Board0: [...], Board1: [...] } }' })
      return
    }

    const wtnPathRel = String(layout.wtn_path || '')
    const wtnPathAbs = path.join(CONFIG_DIR, wtnPathRel)
    await writeWtnFile(wtnPathAbs, boards)

    const reload = await reloadXenwooting()
    res.json({ ok: true, xenwootingReloaded: reload.ok, xenwootingReloadError: reload.error || null })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

// Preview mode: write temporary .wtn without touching the on-disk layout.
app.post(`${API_BASE}/preview/enable`, async (req, res) => {
  try {
    const layoutId = String(req.body?.layoutId || '')
    const boards = req.body?.boards
    if (!layoutId || !boards || !boards.Board0 || !boards.Board1) {
      res.status(400).json({ error: 'Expected body: { layoutId, boards }' })
      return
    }

    await writeFileAtomic(PREVIEW_WTN_PATH, formatWtn(boards))
    await writeFileAtomic(PREVIEW_ENABLED_PATH, `${layoutId}\n`)
    const tm = String(req.body?.trainerMode || '')
    if (tm === 'wait' || tm === 'active') await writeFileAtomic(TRAINER_MODE_PATH, `${tm}\n`)
    else await safeUnlink(TRAINER_MODE_PATH)
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.post(`${API_BASE}/preview/update`, async (req, res) => {
  try {
    const layoutId = String(req.body?.layoutId || '')
    const boards = req.body?.boards
    if (!layoutId || !boards || !boards.Board0 || !boards.Board1) {
      res.status(400).json({ error: 'Expected body: { layoutId, boards }' })
      return
    }
    await writeFileAtomic(PREVIEW_WTN_PATH, formatWtn(boards))
    // keep enabled file updated too
    await writeFileAtomic(PREVIEW_ENABLED_PATH, `${layoutId}\n`)
    const tm = String(req.body?.trainerMode || '')
    if (tm === 'wait' || tm === 'active') await writeFileAtomic(TRAINER_MODE_PATH, `${tm}\n`)
    else await safeUnlink(TRAINER_MODE_PATH)
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.post(`${API_BASE}/preview/disable`, async (_req, res) => {
  try {
    await safeUnlink(PREVIEW_ENABLED_PATH)
    await safeUnlink(PREVIEW_WTN_PATH)
    await safeUnlink(TRAINER_MODE_PATH)
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

// Guide mode: xenwooting-owned chord trainer (physical RGB only).
app.post(`${API_BASE}/guide/start`, async (req, res) => {
  try {
    const layoutId = String(req.body?.layoutId || '')
    const chordKey = String(req.body?.chordKey || '')
    const chordTitle = String(req.body?.chordTitle || '')
    const pcsRoot = Array.isArray(req.body?.pcsRoot) ? req.body.pcsRoot : null
    if (!layoutId || !chordKey || !pcsRoot) {
      res.status(400).json({ error: 'Expected body: { layoutId, chordKey, chordTitle, pcsRoot:number[] }' })
      return
    }
    const pcs = []
    for (const x of pcsRoot) {
      const n = Number.parseInt(String(x), 10)
      if (!Number.isInteger(n)) continue
      pcs.push(n)
    }
    if (pcs.length < 2) {
      res.status(400).json({ error: 'pcsRoot too small' })
      return
    }
    const body = {
      enabled: true,
      layout_id: layoutId,
      chord_key: chordKey,
      chord_title: chordTitle,
      pcs_root: pcs,
      dim: 0.20,
    }
    await writeFileAtomic(GUIDE_PATH, `${JSON.stringify(body)}\n`)
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.post(`${API_BASE}/guide/stop`, async (_req, res) => {
  try {
    await safeUnlink(GUIDE_PATH)
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

// Manual highlight while pointer is held in the web UI.
app.post(`${API_BASE}/highlight`, async (req, res) => {
  try {
    const layoutId = String(req.body?.layoutId || '')
    const board = String(req.body?.board || '')
    const idx = Number.parseInt(String(req.body?.idx ?? ''), 10)
    const down = Boolean(req.body?.down)
    const b = boardNameToIndex(board)
    if (!layoutId || b === null || !Number.isFinite(idx) || idx < 0 || idx >= 56) {
      res.status(400).json({ error: 'Expected body: { layoutId, board: Board0|Board1, idx: 0..55, down: bool }' })
      return
    }

    const text = `layoutId=${layoutId}\nboard=${b}\nidx=${idx}\ndown=${down ? 1 : 0}\nts=${Date.now()}\n`
    await writeFileAtomic(HIGHLIGHT_PATH, text)
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.post(`${API_BASE}/note-names`, async (req, res) => {
  const edo = req.body?.edo
  const pitches = req.body?.pitches
  if (!Number.isInteger(edo) || !Array.isArray(pitches)) {
    res.status(400).json({ error: 'Expected body: { edo: int, pitches: int[] }' })
    return
  }

  const uniq = []
  const seen = new Set()
  for (const p of pitches) {
    if (!Number.isInteger(p)) continue
    const key = `${edo}:${p}`
    if (seen.has(key)) continue
    seen.add(key)
    uniq.push(p)
  }

  const results = {}
  const missing = []
  for (const p of uniq) {
    const key = `${edo}:${p}`
    if (NOTE_NAME_CACHE.has(key)) {
      const v = NOTE_NAME_CACHE.get(key)
      if (v) results[String(p)] = v
    } else {
      missing.push(p)
    }
  }

  if (missing.length === 0) {
    res.json({ edo, results })
    return
  }

  // Proxy to python service; on error, return empty additions.
  try {
    const ac = new AbortController()
    // Note-name generation can be slow for large pitch batches (especially higher EDOs).
    // Keep this comfortably above normal UI burst sizes.
    const t = setTimeout(() => ac.abort(), 5000)
    const r = await fetch(`${XENHARM_URL}/v1/note-names`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ edo, pitches: missing }),
      signal: ac.signal,
    })
    clearTimeout(t)

    if (r.ok) {
      const body = await r.json().catch(() => null)
      const got = body && typeof body === 'object' ? body.results : null
      if (got && typeof got === 'object') {
        for (const p of missing) {
          const k = `${edo}:${p}`
          const vv = got[String(p)]
          if (vv && typeof vv === 'object' && typeof vv.unicode === 'string' && typeof vv.short === 'string') {
            const alts = Array.isArray(vv.alts)
              ? vv.alts
                  .filter((x) => x && typeof x === 'object' && typeof x.short === 'string' && typeof x.unicode === 'string')
                  .map((x) => ({ short: x.short, unicode: x.unicode }))
              : []
            const out = { short: vv.short, unicode: vv.unicode, alts }
            NOTE_NAME_CACHE.set(k, out)
            results[String(p)] = out
          } else {
            NOTE_NAME_CACHE.set(k, null)
          }
        }
      } else {
        for (const p of missing) NOTE_NAME_CACHE.set(`${edo}:${p}`, null)
      }
     }
     // If the proxy failed (timeout, service down, etc) do NOT poison the cache with nulls.
     // Leaving them missing lets the UI retry on the next request.
   } catch {
     // ignore
   }

  res.json({ edo, results })
})

app.post(`${API_BASE}/chord-names`, async (req, res) => {
  const edo = Number.parseInt(String(req.body?.edo ?? ''), 10)
  const pitchClasses = req.body?.pitchClasses
  if (!Number.isInteger(edo) || edo < 1 || edo > 999) {
    res.status(400).json({ error: 'Expected body: { edo: int >= 1, pitchClasses: int[] }' })
    return
  }
  if (!Array.isArray(pitchClasses)) {
    res.status(400).json({ error: 'Expected body: { edo: int, pitchClasses: int[] }' })
    return
  }
  const pcs = []
  for (const pc of pitchClasses) {
    const n = Number.parseInt(String(pc), 10)
    if (!Number.isFinite(n)) continue
    pcs.push(n | 0)
  }
  const results = findChordNames(SCALA_CHORD_DB, edo, pcs)
  res.json({ edo, results })
})

app.get(`${API_BASE}/chord-catalogue`, async (req, res) => {
  const edo = Number.parseInt(String(req.query?.edo ?? ''), 10)
  const limit = Number.parseInt(String(req.query?.limit ?? 250), 10)
  const minTones = Number.parseInt(String(req.query?.minTones ?? 3), 10)
  const maxTones = Number.parseInt(String(req.query?.maxTones ?? 4), 10)

  if (!Number.isInteger(edo) || edo < 1 || edo > 999) {
    res.status(400).json({ error: 'Expected query: edo=int>=1' })
    return
  }
  if (!SCALA_CHORD_DB) {
    res.status(500).json({ error: 'Chord db not loaded' })
    return
  }

  const body = buildChordCatalogue(SCALA_CHORD_DB, edo, { limit, minTones, maxTones })
  res.json(body)
})

app.get(`${API_BASE}/live/state`, async (_req, res) => {
  try {
    const raw = await fs.readFile(LIVE_STATE_PATH, 'utf8')
    const body = JSON.parse(raw)
    res.json(body)
  } catch (e) {
    res.status(404).json({ error: `Live state not available (${LIVE_STATE_PATH})` })
  }
})

// Server-sent events: streams the live state whenever it changes.
app.get(`${API_BASE}/live/stream`, async (req, res) => {
  res.setHeader('Content-Type', 'text/event-stream')
  // Important: SSE must not be transformed or buffered by proxies.
  res.setHeader('Cache-Control', 'no-cache, no-transform')
  // Nginx-style hint; harmless elsewhere.
  res.setHeader('X-Accel-Buffering', 'no')
  res.setHeader('Connection', 'keep-alive')
  res.flushHeaders?.()

  let closed = false
  req.on('close', () => {
    closed = true
  })

  const sendState = async () => {
    try {
      const raw = await fs.readFile(LIVE_STATE_PATH, 'utf8')
      // Validate JSON once so clients don't crash on partial writes.
      const obj = JSON.parse(raw)
      res.write(`event: state\n`)
      res.write(`data: ${JSON.stringify(obj)}\n\n`)
    } catch {
      // If missing/unreadable, just skip.
    }
  }

  // Send immediately.
  await sendState()

  let lastMtimeMs = 0
  const timer = setInterval(async () => {
    if (closed) {
      clearInterval(timer)
      return
    }
    try {
      const st = await fs.stat(LIVE_STATE_PATH)
      const m = Number(st.mtimeMs || 0)
      if (m && m !== lastMtimeMs) {
        lastMtimeMs = m
        await sendState()
      }
    } catch {
      // ignore
    }
  }, 200)
})

app.get(`${API_BASE}/geometry`, async (_req, res) => {
  try {
    const keys = await loadPlayableGeometry(GEOMETRY_JSON)
    const width = keys.reduce((acc, k) => Math.max(acc, k.x + k.w), 0)
    const height = keys.reduce((acc, k) => Math.max(acc, k.y + k.h), 0)
    res.json({ source: GEOMETRY_JSON, width, height, keys })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.use(APP_BASE, express.static(DIST_DIR))
app.get(`${APP_BASE}/*`, (_req, res) => {
  res.sendFile(path.join(DIST_DIR, 'index.html'))
})

app.listen(PORT, () => {
  // eslint-disable-next-line no-console
  console.log(`WTN editor server listening on http://0.0.0.0:${PORT}${APP_BASE}/`)
})
