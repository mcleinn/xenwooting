import express from 'express'
import path from 'node:path'
import fs from 'node:fs/promises'
import TOML from '@iarna/toml'

import { formatWtn, readWtnFile, writeWtnFile } from './wtn.js'
import { loadPlayableGeometry } from './geometry.js'

const APP_BASE = '/wtn'
const API_BASE = `${APP_BASE}/api`

const CONFIG_DIR = process.env.XENWOOTING_CONFIG_DIR || '/home/patch/.config/xenwooting'
const CONFIG_TOML = process.env.XENWOOTING_CONFIG_TOML || path.join(CONFIG_DIR, 'config.toml')
const XEN_WOOTING_JSON = process.env.XENWOOTING_GEOMETRY_JSON || '/home/patch/xenWooting.json'
const PORT = Number.parseInt(process.env.PORT || '3174', 10)

const DIST_DIR = path.resolve(new URL('../web/dist', import.meta.url).pathname)

const app = express()
app.use(express.json({ limit: '2mb' }))

async function reloadXenwooting() {
  // xenwooting watches the .wtn file and reloads on change.
  return { ok: true }
}

const PREVIEW_ENABLED_PATH = process.env.XENWOOTING_PREVIEW_ENABLED_PATH || '/tmp/xenwooting-preview.enabled'
const PREVIEW_WTN_PATH = process.env.XENWOOTING_PREVIEW_WTN_PATH || '/tmp/xenwooting-preview.wtn'
const HIGHLIGHT_PATH = process.env.XENWOOTING_HIGHLIGHT_PATH || '/tmp/xenwooting-highlight.txt'

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

    res.json({
      configDir: CONFIG_DIR,
      layouts: layouts
        .map((l) => ({
          id: String(l.id || ''),
          name: String(l.name || l.id || ''),
          wtnPath: String(l.wtn_path || ''),
        }))
        .filter((l) => l.id && l.wtnPath),
    })
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
    res.json({ ok: true })
  } catch (err) {
    res.status(500).json({ error: String(err?.message || err) })
  }
})

app.post(`${API_BASE}/preview/disable`, async (_req, res) => {
  try {
    await safeUnlink(PREVIEW_ENABLED_PATH)
    await safeUnlink(PREVIEW_WTN_PATH)
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

app.get(`${API_BASE}/geometry`, async (_req, res) => {
  try {
    const keys = await loadPlayableGeometry(XEN_WOOTING_JSON)
    const width = keys.reduce((acc, k) => Math.max(acc, k.x + k.w), 0)
    const height = keys.reduce((acc, k) => Math.max(acc, k.y + k.h), 0)
    res.json({ source: XEN_WOOTING_JSON, width, height, keys })
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
