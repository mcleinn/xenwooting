import './App.css'
import { useEffect, useMemo, useRef, useState } from 'react'
import {
  fetchGeometry,
  fetchLayout,
  fetchLayouts,
  highlightKey,
  previewDisable,
  previewEnable,
  previewUpdate,
  saveLayout,
} from './api'
import { KeyboardView } from './KeyboardView'
import type { Boards, Geometry, LayoutInfo } from './types'
import { parseLtnText, placeLtnKeys, type LtnPlacedKey } from './ltn'

const C0_HZ = 16.351_597_831_287_414

function App() {
  const [layouts, setLayouts] = useState<LayoutInfo[]>([])
  const [layoutId, setLayoutId] = useState<string>('')
  const [layoutName, setLayoutName] = useState<string>('')
  const [edoDivisions, setEdoDivisions] = useState<number>(12)
  const [pitchOffset, setPitchOffset] = useState<number>(0)
  const [geometry, setGeometry] = useState<Geometry | null>(null)
  const [boards, setBoards] = useState<Boards | null>(null)
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [selectedOrder, setSelectedOrder] = useState<string[]>([])
  const [lastSelected, setLastSelected] = useState<string | null>(null)

  const [editNote, setEditNote] = useState<string>('')
  const [editChan, setEditChan] = useState<string>('')
  const [editColText, setEditColText] = useState<string>('#ffffff')
  const [editColPick, setEditColPick] = useState<string>('#ffffff')
  const [noteMixed, setNoteMixed] = useState(false)
  const [chanMixed, setChanMixed] = useState(false)
  const [colMixed, setColMixed] = useState(false)
  const [status, setStatus] = useState<string>('')

  const [previewMode, setPreviewMode] = useState(false)
  const previewPushTimer = useRef<number | null>(null)

  const fileInputRef = useRef<HTMLInputElement | null>(null)
  const [importActive, setImportActive] = useState(false)
  const [importKeys, setImportKeys] = useState<LtnPlacedKey[] | null>(null)
  const [importDx2, setImportDx2] = useState(0)
  const [importDy, setImportDy] = useState(0)
  const [interpMode, setInterpMode] = useState(false)

  useEffect(() => {
    let cancelled = false
    Promise.all([fetchLayouts(), fetchGeometry()])
      .then(([l, g]) => {
        if (cancelled) return
        setLayouts(l.layouts)
        setGeometry(g)
        if (l.layouts.length) {
          setLayoutId((prev) => prev || l.layouts[0].id)
        }
      })
      .catch((e) => {
        if (cancelled) return
        setStatus(`Load failed: ${errMsg(e)}`)
      })
    return () => {
      cancelled = true
    }
  }, [])

  useEffect(() => {
    if (!layoutId) return
    let cancelled = false
    setStatus('Loading .wtn...')

    // Changing layout exits preview mode.
    if (previewMode) {
      setPreviewMode(false)
      previewDisable().catch(() => {})
    }

    fetchLayout(layoutId)
      .then((l) => {
        if (cancelled) return
        setLayoutName(l.name)
        setBoards(l.boards)
        setEdoDivisions(Number.isFinite(l.edoDivisions) ? l.edoDivisions : 12)
        setPitchOffset(Number.isFinite(l.pitchOffset) ? l.pitchOffset : 0)
        setSelected(new Set())
        setSelectedOrder([])
        setLastSelected(null)
        setStatus('')
      })
      .catch((e) => {
        if (cancelled) return
        setStatus(`Load failed: ${errMsg(e)}`)
      })
    return () => {
      cancelled = true
    }
  }, [layoutId])

  // Escape cancels import placement.
  useEffect(() => {
    if (!importActive) return
    const onKeyDown = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase()
      if (tag === 'input' || tag === 'textarea' || tag === 'select') return

      if (e.key === 'Escape') {
        e.preventDefault()
        exitImportMode()
        setStatus('')
        return
      }

      if (e.key === 'Enter') {
        e.preventDefault()
        onImportApply()
        return
      }

      if (e.key === 'i' || e.key === 'I') {
        e.preventDefault()
        setInterpMode((v) => !v)
        return
      }

      if (e.key === 'ArrowLeft') {
        e.preventDefault()
        setImportDx2((v) => v - 2)
        return
      }
      if (e.key === 'ArrowRight') {
        e.preventDefault()
        setImportDx2((v) => v + 2)
        return
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault()
        setImportDy((v) => v - 1)
        return
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        setImportDy((v) => v + 1)
        return
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [importActive, boards, geometry, importKeys, importDx2, importDy, previewMode, layoutId])

  const selectionCount = selected.size
  const selectedCells = useMemo(() => {
    if (!boards) return []
    const out: Array<{ board: 'Board0' | 'Board1'; idx: number }> = []
    for (const id of selected) {
      const [b, i] = id.split(':')
      if (b !== 'Board0' && b !== 'Board1') continue
      const idx = Number.parseInt(i || '', 10)
      if (!Number.isFinite(idx)) continue
      out.push({ board: b, idx })
    }
    return out
  }, [selected, boards])

  const selectedCellsInfo = useMemo(() => {
    if (!boards) return []
    const out: Array<{ id: string; pitch: number; hz: number }> = []

    const edo = Math.max(1, edoDivisions)

    for (const id of selected) {
      const [b, i] = id.split(':')
      if (b !== 'Board0' && b !== 'Board1') continue
      const idx = Number.parseInt(i || '', 10)
      if (!Number.isFinite(idx)) continue
      const cell = boards[b][idx]
      if (!cell) continue

      const ch0 = Math.max(0, Math.min(15, cell.chan - 1))
      const pitch = ch0 * edo + cell.note + pitchOffset
      const hz = Math.round(C0_HZ * Math.pow(2, pitch / edo))
      out.push({ id, pitch, hz })
    }

    return out
  }, [boards, selected, edoDivisions, pitchOffset])

  // Keep editor fields in sync with selection.
  // - Single key: show its values.
  // - Multi select: show values only if common to all selected.
  // - Mixed: clear the field (placeholder shows "mixed").
  useEffect(() => {
    if (!boards) return

    if (selectedCells.length === 0) {
      setNoteMixed(false)
      setChanMixed(false)
      setColMixed(false)
      return
    }

    const first = selectedCells[0]
    const firstCell = boards[first.board][first.idx]
    if (!firstCell) return

    let commonNote: number | null = firstCell.note
    let commonChan: number | null = firstCell.chan
    let commonCol: string | null = firstCell.col

    for (const s of selectedCells) {
      const cell = boards[s.board][s.idx]
      if (!cell) continue
      if (commonNote !== null && cell.note !== commonNote) commonNote = null
      if (commonChan !== null && cell.chan !== commonChan) commonChan = null
      if (commonCol !== null && cell.col !== commonCol) commonCol = null
    }

    setNoteMixed(commonNote === null)
    setChanMixed(commonChan === null)
    setColMixed(commonCol === null)

    setEditNote(commonNote === null ? '' : String(commonNote))
    setEditChan(commonChan === null ? '' : String(commonChan))
    if (commonCol === null) {
      setEditColText('')
    } else {
      const v = `#${commonCol.toLowerCase()}`
      setEditColText(v)
      setEditColPick(v)
    }
  }, [boards, selectedCells])

  function applyEdits() {
    if (!boards) return

    const note = editNote.trim() === '' ? null : clampInt(editNote, 0, 127)
    const chan = editChan.trim() === '' ? null : clampInt(editChan, 1, 16)
    const col = normalizeHex6(editColText)

    const next: Boards = {
      Board0: boards.Board0.map((c) => ({ ...c })),
      Board1: boards.Board1.map((c) => ({ ...c })),
    }

    for (const { board, idx } of selectedCells) {
      const cell = next[board][idx]
      if (!cell) continue
      if (note !== null) cell.note = note
      if (chan !== null) cell.chan = chan
      if (col) cell.col = col
    }

    setBoards(next)
    pushPreview(next)
  }

  function applyOctave() {
    if (!boards) return
    if (selectedOrder.length === 0) return

    const next: Boards = {
      Board0: boards.Board0.map((c) => ({ ...c })),
      Board1: boards.Board1.map((c) => ({ ...c })),
    }

    let n = 1
    for (const id of selectedOrder) {
      if (!selected.has(id)) continue
      const [b, i] = id.split(':')
      if (b !== 'Board0' && b !== 'Board1') continue
      const idx = Number.parseInt(i || '', 10)
      if (!Number.isFinite(idx)) continue
      const cell = next[b][idx]
      if (!cell) continue
      cell.note = Math.min(127, Math.max(0, n))
      n += 1
    }

    setBoards(next)
    pushPreview(next)
  }

  function pushPreview(nextBoards: Boards) {
    if (!previewMode) return
    if (!layoutId) return

    if (previewPushTimer.current !== null) {
      window.clearTimeout(previewPushTimer.current)
      previewPushTimer.current = null
    }
    previewPushTimer.current = window.setTimeout(() => {
      previewUpdate(layoutId, nextBoards).catch((e) => {
        setStatus(`Preview sync failed: ${errMsg(e)}`)
      })
    }, 60)
  }

  function exitImportMode() {
    setImportActive(false)
    setImportKeys(null)
    setImportDx2(0)
    setImportDy(0)
    setInterpMode(false)
  }

  async function exitPreviewMode() {
    setPreviewMode(false)
    try {
      await previewDisable()
    } catch (e) {
      setStatus(`Preview disable failed: ${errMsg(e)}`)
    }
  }

  async function onTogglePreview(next: boolean) {
    if (!layoutId || !boards) return
    if (!next) {
      await exitPreviewMode()
      return
    }
    setStatus('Preview enabling...')
    try {
      await previewEnable(layoutId, boards)
      setPreviewMode(true)
      setStatus('Preview enabled.')
      setTimeout(() => setStatus(''), 800)
    } catch (e) {
      setPreviewMode(false)
      setStatus(`Preview enable failed: ${errMsg(e)}`)
    }
  }

  async function onRevert() {
    exitImportMode()
    await exitPreviewMode()
    if (!layoutId) return
    setStatus('Reverting...')
    try {
      const l = await fetchLayout(layoutId)
      setLayoutName(l.name)
      setBoards(l.boards)
      setEdoDivisions(Number.isFinite(l.edoDivisions) ? l.edoDivisions : 12)
      setPitchOffset(Number.isFinite(l.pitchOffset) ? l.pitchOffset : 0)
      setSelected(new Set())
      setSelectedOrder([])
      setLastSelected(null)
      setStatus('Reverted.')
      setTimeout(() => setStatus(''), 800)
    } catch (e) {
      setStatus(`Revert failed: ${errMsg(e)}`)
    }
  }

  async function onSave() {
    if (!boards) return
    setStatus('Saving...')
    try {
      const r = await saveLayout(layoutId, boards)
      exitImportMode()
      await exitPreviewMode()
      setStatus(r.xenwootingReloaded ? 'Saved + reloaded XenWooting.' : 'Saved (reload failed).')
      // Refresh from disk so UI matches canonical .wtn.
      const l = await fetchLayout(layoutId)
      setLayoutName(l.name)
      setBoards(l.boards)
      setEdoDivisions(Number.isFinite(l.edoDivisions) ? l.edoDivisions : 12)
      setPitchOffset(Number.isFinite(l.pitchOffset) ? l.pitchOffset : 0)

      if (!r.xenwootingReloaded && r.xenwootingReloadError) {
        setTimeout(() => setStatus(`Saved (reload failed): ${r.xenwootingReloadError}`), 400)
      } else {
        setTimeout(() => setStatus(''), 1400)
      }
    } catch (e) {
      setStatus(`Save failed: ${errMsg(e)}`)
    }
  }

  function onKeyHighlight(board: 'Board0' | 'Board1', idx: number, down: boolean) {
    if (!layoutId) return
    highlightKey(layoutId, board, idx, down).catch(() => {})
  }

  function onImportClick() {
    if (!fileInputRef.current) return
    fileInputRef.current.value = ''
    fileInputRef.current.click()
  }

  async function onImportFileSelected(file: File | null) {
    if (!file || !boards) return
    exitImportMode()
    setStatus('Importing .ltn...')
    try {
      const text = await file.text()
      const ltn = parseLtnText(text)
      const placed = placeLtnKeys(ltn)
      setImportKeys(placed)
      setImportActive(true)
      setImportDx2(0)
      setImportDy(0)
      setStatus('')
    } catch (e) {
      setStatus(`Import failed: ${errMsg(e)}`)
    }
  }

  const importOverlay = useMemo(() => {
    if (!importActive || !importKeys || !geometry || !boards) return null

    const overlayByBoard = computeOverlayByBoard(importKeys, importDx2, importDy)
    const suggestedByBoard = interpMode ? computeSuggestedByBoard(overlayByBoard) : null
    return { overlayByBoard, suggestedByBoard }
  }, [importActive, importKeys, importDx2, importDy, geometry, boards, interpMode])

  function onImportApply() {
    if (!boards || !importKeys || !geometry) return
    const next: Boards = {
      Board0: boards.Board0.map((c) => ({ ...c })),
      Board1: boards.Board1.map((c) => ({ ...c })),
    }

    const overlayByBoard = computeOverlayByBoard(importKeys, importDx2, importDy)
    const suggestedByBoard = interpMode ? computeSuggestedByBoard(overlayByBoard) : null

    for (const board of ['Board0', 'Board1'] as const) {
      for (const [idx, v] of overlayByBoard[board].entries()) {
        const cell = next[board][idx]
        if (!cell) continue
        cell.note = v.note
        cell.chan = v.chan
        cell.col = v.col
      }
      if (suggestedByBoard) {
        for (const [idx, v] of suggestedByBoard[board].entries()) {
          const cell = next[board][idx]
          if (!cell) continue
          cell.note = v.note
          cell.chan = v.chan
          cell.col = v.col
        }
      }
    }

    setBoards(next)
    pushPreview(next)
    exitImportMode()
    setStatus('Import applied (not saved).')
    setTimeout(() => setStatus(''), 1000)
  }

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brandTitle">XenWooting configurator</div>
        </div>

        <div className="controls">
          <button className="btnSecondary" type="button" onClick={onImportClick} disabled={!boards || !layoutId}>
            Import
          </button>

          <input
            ref={fileInputRef}
            type="file"
            accept=".ltn"
            style={{ display: 'none' }}
            onChange={(e) => void onImportFileSelected(e.target.files?.[0] || null)}
          />

          <label className="previewToggle">
            <input
              type="checkbox"
              checked={previewMode}
              onChange={(e) => void onTogglePreview(e.target.checked)}
              disabled={!boards || !layoutId}
            />
            <span>Preview mode</span>
          </label>

          <label className="field">
            <span className="fieldLabel">Layout</span>
            <select
              className="select"
              value={layoutId}
              onChange={(e) => setLayoutId(e.target.value)}
              disabled={!layouts.length}
            >
              {layouts.map((l) => (
                <option key={l.id} value={l.id}>
                  {l.name}
                </option>
              ))}
            </select>
          </label>

          <button
            className="btnSecondary"
            type="button"
            onClick={() => void onRevert()}
            disabled={!layoutId}
          >
            Revert
          </button>

          <button className="btn" type="button" onClick={onSave} disabled={!boards || !layoutId}>
            Save
          </button>
        </div>
      </header>

      <main className="main">
        <div className="sidebar">
          <section className="panel">
            <div className="panelTitle">Edit Selection</div>
            <div className="panelSub">
              {layoutName ? layoutName : '—'}; selected {selectionCount}
            </div>

            <div className="grid">
              <label className="field">
                <span className="fieldLabel">Channel (1-16)</span>
                <input
                  className="input"
                  inputMode="numeric"
                  value={editChan}
                  onChange={(e) => setEditChan(e.target.value)}
                  placeholder={selectionCount === 0 ? '(select)' : chanMixed ? '(mixed)' : '(keep)'}
                />
              </label>

              <label className="field">
                <span className="fieldLabel">Note (0-127)</span>
                <input
                  className="input"
                  inputMode="numeric"
                  value={editNote}
                  onChange={(e) => setEditNote(e.target.value)}
                  placeholder={selectionCount === 0 ? '(select)' : noteMixed ? '(mixed)' : '(keep)'}
                />
              </label>

              <label className="field">
                <span className="fieldLabel">Color</span>
                <div className="colorRow">
                <input
                  className="color"
                  type="color"
                  value={editColPick}
                  onPointerDown={() => {
                    // If the selection is mixed, let the swatch "seed" a concrete value
                    // into the text field so Apply will set all selected cells.
                    if (colMixed || editColText.trim() === '') {
                      setColMixed(false)
                      setEditColText(editColPick)
                    }
                  }}
                  onChange={(e) => {
                    setColMixed(false)
                    setEditColPick(e.target.value)
                    setEditColText(e.target.value)
                  }}
                />
                  <input
                    className="input"
                    value={editColText}
                    placeholder={selectionCount === 0 ? '(select)' : colMixed ? '(mixed)' : '(keep)'}
                    onChange={(e) => {
                      const v = e.target.value
                      setEditColText(v)
                      const norm = normalizeHex7(v)
                      if (norm) {
                        setColMixed(false)
                        setEditColPick(norm)
                      }
                    }}
                  />
                </div>
              </label>
            </div>

            <div className="row">
              <button
                className="btnSecondary"
                type="button"
                onClick={applyEdits}
                disabled={!boards || selected.size === 0}
              >
                Apply
              </button>
              <button
                className="btnSecondary"
                type="button"
                onClick={applyOctave}
                disabled={!boards || selectedOrder.length === 0}
              >
                Octave
              </button>
              <button
                className="btnSecondary"
                type="button"
                onClick={() => {
                  setSelected(new Set())
                  setSelectedOrder([])
                  setLastSelected(null)
                }}
                disabled={selected.size === 0}
              >
                Clear
              </button>
            </div>

            {status && <div className="status">{status}</div>}
            {importActive && (
              <div className="status">
                Use arrow keys to place, ESC to abort placement. RETURN to apply placement. I toggles interpolation.
              </div>
            )}
            <div className="hint">
              Click to select. Ctrl/Meta to multi-select. Shift to add range. Blank fields keep values.
            </div>
          </section>

          <section className="panel">
            <div className="panelTitle">Selected Cells</div>
            <div className="panelSub">
              pitch = (ch-1)*{edoDivisions} + note + {pitchOffset}
            </div>
            {selectionCount === 0 ? (
              <div className="hint">No selection.</div>
            ) : (
              <ul className="cellList">
                {selectedCellsInfo.map((x) => (
                  <li key={x.id} className="cellLine">
                    <span className="cellId">{x.id}</span>
                    <span className="cellVal">pitch {x.pitch}</span>
                    <span className="cellVal">{x.hz} Hz</span>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>

        <section className="boards">
          {!geometry || !boards ? (
            <div className="loading">Loading…</div>
          ) : (
            <>
              {importActive && (
                <div
                  className="importOverlay"
                  onPointerDown={(e) => e.preventDefault()}
                />
              )}
              <KeyboardView
                title="Board0"
                boardId="Board0"
                geometry={geometry}
                cells={boards.Board0}
                rotate180
                xOffsetU={3}
                selected={selected}
                selectedOrder={selectedOrder}
                setSelected={setSelected}
                setSelectedOrder={setSelectedOrder}
                lastSelected={lastSelected}
                setLastSelected={setLastSelected}
                onKeyHighlight={importActive ? undefined : onKeyHighlight}
                overlayByIdx={importOverlay?.overlayByBoard.Board0}
                suggestedByIdx={importOverlay?.suggestedByBoard?.Board0 || undefined}
              />
              <KeyboardView
                title="Board1"
                boardId="Board1"
                geometry={geometry}
                cells={boards.Board1}
                selected={selected}
                selectedOrder={selectedOrder}
                setSelected={setSelected}
                setSelectedOrder={setSelectedOrder}
                lastSelected={lastSelected}
                setLastSelected={setLastSelected}
                onKeyHighlight={importActive ? undefined : onKeyHighlight}
                overlayByIdx={importOverlay?.overlayByBoard.Board1}
                suggestedByIdx={importOverlay?.suggestedByBoard?.Board1 || undefined}
              />
            </>
          )}
        </section>
      </main>
    </div>
  )
}

// (old nearest-target placement helpers removed; we now do contiguous row placement)

type CellDef = { note: number; chan: number; col: string }
type OverlayByBoard = { Board0: Map<number, CellDef>; Board1: Map<number, CellDef> }

function computeOverlayByBoard(importKeys: LtnPlacedKey[], dx2: number, dy: number): OverlayByBoard {
  const byBoard: OverlayByBoard = { Board0: new Map(), Board1: new Map() }

  // Group by integer lum row (after vertical offset).
  const rows = new Map<number, LtnPlacedKey[]>()
  for (const k of importKeys) {
    const y = k.y + dy
    const arr = rows.get(y) || []
    arr.push(k)
    rows.set(y, arr)
  }

  const candidates = buildWootingRowCandidates()

  for (const [yRow, keys] of rows.entries()) {
    const sorted = [...keys].sort((a, b) => (a.x2 + dx2) - (b.x2 + dx2))
    if (sorted.length === 0) continue

    let best: {
      score: number
      cand: (typeof candidates)[number]
      winStart: number
      startCol: number
    } | null = null

    for (const cand of candidates) {
      const n = sorted.length
      const len = cand.len
      const winSize = Math.min(n, len)
      if (winSize <= 0) continue

      // If overflow: try all windows of length len and pick best.
      const winCount = n > len ? n - len + 1 : 1
      for (let w0 = 0; w0 < winCount; w0++) {
        const win = sorted.slice(w0, w0 + winSize)
        const lumCy = yRow + 0.5
        const dyAbs = Math.abs(lumCy - cand.cy)

        const s = bestStartCol(win, dx2, cand.xOffU, len)
        if (s === null) continue

        let score = 0
        for (let i = 0; i < win.length; i++) {
          const px2 = win[i].x2 + dx2
          const targetCx2 = 2 * (cand.xOffU + (s + i)) + 1
          const dxU = Math.abs(px2 - targetCx2) / 2
          score += dyAbs * 6 + dxU
        }

        if (!best || score < best.score) {
          best = { score, cand, winStart: w0, startCol: s }
        }
      }
    }

    if (!best) continue
    const win = sorted.slice(best.winStart, best.winStart + Math.min(sorted.length, best.cand.len))
    for (let i = 0; i < win.length; i++) {
      const idx = best.cand.row * 14 + (best.startCol + i)
      if (idx < 0 || idx >= 56) continue
      byBoard[best.cand.board].set(idx, {
        note: win[i].cell.note,
        chan: win[i].cell.chan,
        col: win[i].cell.col,
      })
    }
  }

  return byBoard
}

function buildWootingRowCandidates() {
  const rowLens = [14, 14, 13, 12]
  const out: Array<{ board: 'Board0' | 'Board1'; row: number; len: number; xOffU: number; cy: number }> = []
  for (const board of ['Board0', 'Board1'] as const) {
    const xOffU = board === 'Board0' ? 3 : 0
    const yOff = board === 'Board1' ? 4 : 0
    for (let r = 0; r < 4; r++) {
      out.push({ board, row: r, len: rowLens[r], xOffU, cy: r + yOff + 0.5 })
    }
  }
  return out
}

function bestStartCol(win: LtnPlacedKey[], dx2: number, xOffU: number, len: number) {
  const m = win.length
  if (m <= 0) return null
  const sList: number[] = []
  for (let i = 0; i < m; i++) {
    const px2 = win[i].x2 + dx2
    const cEst = Math.round((px2 - 1) / 2 - xOffU)
    sList.push(cEst - i)
  }
  sList.sort((a, b) => a - b)
  let s = sList[Math.floor(sList.length / 2)]
  s = Math.min(len - m, Math.max(0, s))
  return s
}

function computeSuggestedByBoard(overlayByBoard: OverlayByBoard): OverlayByBoard {
  // Suggest across the combined grid row (Board0 row r shares a "combined row" with Board1 row r-4).
  // This ensures every non-overlayed key in a row gets a suggestion, even if the overlay data
  // for that row sits only on the other board.
  const rowLens = [14, 14, 13, 12]
  const out: OverlayByBoard = { Board0: new Map(), Board1: new Map() }

  type KeyPos = { board: 'Board0' | 'Board1'; r: number; c: number; idx: number; gx: number }
  type Seed = { gx: number; v: CellDef }

  const positionsByCombinedRow = new Map<number, KeyPos[]>()
  for (const board of ['Board0', 'Board1'] as const) {
    const xOff = board === 'Board0' ? 3 : 0
    const yOff = board === 'Board1' ? 4 : 0
    for (let r = 0; r < 4; r++) {
      const len = rowLens[r]
      const crow = r + yOff
      const arr = positionsByCombinedRow.get(crow) || []
      for (let c = 0; c < len; c++) {
        const idx = r * 14 + c
        arr.push({ board, r, c, idx, gx: c + xOff })
      }
      positionsByCombinedRow.set(crow, arr)
    }
  }

  for (const positions of positionsByCombinedRow.values()) {
    const seeds: Seed[] = []
    for (const p of positions) {
      const v = overlayByBoard[p.board].get(p.idx)
      if (v) seeds.push({ gx: p.gx, v })
    }
    if (seeds.length === 0) continue
    seeds.sort((a, b) => a.gx - b.gx)

    for (const p of positions) {
      if (overlayByBoard[p.board].has(p.idx)) continue

      const left = findLeftSeed(seeds, p.gx)
      const right = findRightSeed(seeds, p.gx)
      if (!left && !right) continue

      const pickColor = (() => {
        if (left && right) {
          return p.gx - left.gx <= right.gx - p.gx ? left.v.col : right.v.col
        }
        return (left || right)!.v.col
      })()

      const interp = (() => {
        if (left && right && right.gx !== left.gx) {
          const t = (p.gx - left.gx) / (right.gx - left.gx)
          const note = Math.round(left.v.note * (1 - t) + right.v.note * t)
          const chan = Math.round(left.v.chan * (1 - t) + right.v.chan * t)
          return { note, chan }
        }
        const v = (left || right)!.v
        return { note: v.note, chan: v.chan }
      })()

      out[p.board].set(p.idx, {
        note: modNote(interp.note),
        chan: modChan(interp.chan),
        col: pickColor,
      })
    }
  }

  return out
}

// (old per-row helpers removed; interpolation uses gx-based helpers)

function findLeftSeed(arr: Array<{ gx: number; v: CellDef }>, gx: number) {
  for (let i = arr.length - 1; i >= 0; i--) {
    if (arr[i].gx < gx) return arr[i]
  }
  return null
}

function findRightSeed(arr: Array<{ gx: number; v: CellDef }>, gx: number) {
  for (let i = 0; i < arr.length; i++) {
    if (arr[i].gx > gx) return arr[i]
  }
  return null
}

function modChan(chan1: number) {
  const c0 = ((Math.trunc(chan1) - 1) % 16 + 16) % 16
  return c0 + 1
}

function modNote(note: number) {
  const n0 = ((Math.trunc(note) % 128) + 128) % 128
  return n0
}

export default App

function clampInt(s: string, min: number, max: number) {
  const n = Number.parseInt(s, 10)
  if (!Number.isFinite(n)) return min
  return Math.min(max, Math.max(min, n))
}

function normalizeHex6(s: string) {
  const v = s.trim().replace(/^#/, '').toUpperCase()
  if (/^[0-9A-F]{6}$/.test(v)) return v
  return null
}

function normalizeHex7(s: string) {
  const v = s.trim()
  if (/^#[0-9A-Fa-f]{6}$/.test(v)) return v.toLowerCase()
  const hex6 = normalizeHex6(v)
  return hex6 ? `#${hex6.toLowerCase()}` : null
}

function errMsg(e: unknown) {
  if (e instanceof Error) return e.message
  return String(e)
}
