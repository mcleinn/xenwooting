import './App.css'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  fetchGeometry,
  fetchLayout,
  fetchLayouts,
  fetchNoteNames,
  highlightKey,
  previewDisable,
  previewEnable,
  previewUpdate,
  saveLayout,
} from './api'
import { KeyboardView } from './KeyboardView'
import type { Boards, Geometry, LayoutInfo } from './types'

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

  const [displayMode, setDisplayMode] = useState<'label' | 'number' | 'both'>('both')

  const [enumOpen, setEnumOpen] = useState(false)
  const [enumInc, setEnumInc] = useState('1')
  const enumInputRef = useRef<HTMLInputElement | null>(null)

  const [hoveredKey, setHoveredKey] = useState<{ board: 'Board0' | 'Board1'; idx: number } | null>(null)

  const [previewMode, setPreviewMode] = useState(false)
  const previewPushTimer = useRef<number | null>(null)

  const pushPreview = useCallback(
    (nextBoards: Boards) => {
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
    },
    [layoutId, previewMode],
  )

  // Note names (unicode) via xenharm service (proxied by node backend).
  // key: `${edo}:${pitch}` -> { short, unicode } | null
  const [noteNameCache, setNoteNameCache] = useState<Map<string, { short: string; unicode: string } | null>>(new Map())
  const noteNamesFetchTimer = useRef<number | null>(null)

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
    // eslint-disable-next-line react-hooks/set-state-in-effect
    setStatus('Loading .wtn...')

    // Changing layout exits preview mode.
    setPreviewMode(false)
    previewDisable().catch(() => {})

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

  useEffect(() => {
    if (!enumOpen) return
    const t = window.setTimeout(() => {
      enumInputRef.current?.focus()
      enumInputRef.current?.select()
    }, 0)
    return () => window.clearTimeout(t)
  }, [enumOpen])

  // Hotkey: 'c' cycles color of hovered key only.
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase()
      if (tag === 'input' || tag === 'textarea' || tag === 'select') return
      if (e.key.toLowerCase() !== 'c') return
      if (!boards) return
      if (!hoveredKey) return
      if (!geometry) return

      // Palette: colors from actually drawn keys only (no holes).
      const palette: string[] = []
      const seen = new Set<string>()
      const vis0 = visibleWtnIdxs(geometry, true)
      const vis1 = visibleWtnIdxs(geometry, false)
      for (const [b, idxs] of [
        ['Board0', vis0],
        ['Board1', vis1],
      ] as const) {
        for (const i of idxs) {
          const c = boards[b][i]
          if (!c) continue
          const col = String(c.col || '').trim().toUpperCase()
          if (!/^[0-9A-F]{6}$/.test(col)) continue
          if (seen.has(col)) continue
          seen.add(col)
          palette.push(col)
        }
      }
      if (palette.length < 2) return

      const { board, idx } = hoveredKey
      const cur = boards[board]?.[idx]
      if (!cur) return
      const curCol = String(cur.col || '').trim().toUpperCase()
      const at = palette.indexOf(curCol)
      const nextCol = palette[(at >= 0 ? at + 1 : 0) % palette.length]
      if (!nextCol || nextCol === cur.col) return

      const next: Boards = {
        Board0: boards.Board0.map((c) => ({ ...c })),
        Board1: boards.Board1.map((c) => ({ ...c })),
      }
      const cell = next[board][idx]
      if (!cell) return
      cell.col = nextCol
      setBoards(next)
      pushPreview(next)

      e.preventDefault()
    }

    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [boards, hoveredKey, geometry, pushPreview])

  // Prefetch note names for all keys (both boards) and cache them.
  useEffect(() => {
    if (!boards) return
    const edo = Math.max(1, edoDivisions)

    const missing: number[] = []
    const seenPitch = new Set<number>()
    for (const b of ['Board0', 'Board1'] as const) {
      for (const c of boards[b]) {
        const pitch = (Math.max(0, Math.min(15, c.chan - 1)) * edo + c.note + pitchOffset) | 0
        if (seenPitch.has(pitch)) continue
        seenPitch.add(pitch)

        const k = `${edo}:${pitch}`
        if (!noteNameCache.has(k)) missing.push(pitch)
      }
    }

    if (missing.length === 0) return

    // Debounce so editing doesn't spam the server.
    if (noteNamesFetchTimer.current !== null) {
      window.clearTimeout(noteNamesFetchTimer.current)
      noteNamesFetchTimer.current = null
    }
    noteNamesFetchTimer.current = window.setTimeout(() => {
      fetchNoteNames(edo, missing)
        .then((r) => {
          const results = r?.results || {}
          setNoteNameCache((prev) => {
            const next = new Map(prev)
            for (const p of missing) {
              const v = results[String(p)]
              const key = `${edo}:${p}`
              if (v && typeof v.unicode === 'string' && typeof v.short === 'string') {
                next.set(key, { short: v.short, unicode: v.unicode })
              } else {
                next.set(key, null)
              }
            }
            return next
          })
        })
        .catch(() => {
          setNoteNameCache((prev) => {
            const next = new Map(prev)
            for (const p of missing) next.set(`${edo}:${p}`, null)
            return next
          })
        })
    }, 80)

    return () => {
      if (noteNamesFetchTimer.current !== null) {
        window.clearTimeout(noteNamesFetchTimer.current)
        noteNamesFetchTimer.current = null
      }
    }
  }, [boards, edoDivisions, pitchOffset, noteNameCache])

  const noteNamesByBoard = useMemo(() => {
    if (!boards) return null
    const edo = Math.max(1, edoDivisions)
    const b0 = new Map<number, string>()
    const b1 = new Map<number, string>()
    for (const [b, out] of [
      ['Board0', b0],
      ['Board1', b1],
    ] as const) {
      for (let i = 0; i < boards[b].length; i++) {
        const c = boards[b][i]
        const pitch = (Math.max(0, Math.min(15, c.chan - 1)) * edo + c.note + pitchOffset) | 0
        const v = noteNameCache.get(`${edo}:${pitch}`)
        if (v && v.unicode) out.set(i, v.unicode)
      }
    }
    return { Board0: b0, Board1: b1 }
  }, [boards, edoDivisions, pitchOffset, noteNameCache])

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
    const out: Array<{ id: string; pitch: number; hz: number; unicode: string }> = []

    const edo = Math.max(1, edoDivisions)

    for (const id of selected) {
      const [b, i] = id.split(':')
      if (b !== 'Board0' && b !== 'Board1') continue
      const idx = Number.parseInt(i || '', 10)
      if (!Number.isFinite(idx)) continue
      const cell = boards[b][idx]
      if (!cell) continue

      const ch0 = Math.max(0, Math.min(15, cell.chan - 1))
      const pitch = (ch0 * edo + cell.note + pitchOffset) | 0
      const hz = Math.round(C0_HZ * Math.pow(2, pitch / edo))

      const nn = noteNameCache.get(`${edo}:${pitch}`)
      const unicode = nn && nn.unicode ? nn.unicode : ''
      out.push({ id, pitch, hz, unicode })
    }

    return out
  }, [boards, selected, edoDivisions, pitchOffset, noteNameCache])

  // Keep editor fields in sync with selection.
  // - Single key: show its values.
  // - Multi select: show values only if common to all selected.
  // - Mixed: clear the field (placeholder shows "mixed").
  useEffect(() => {
    if (!boards) return

    if (selectedCells.length === 0) {
      // eslint-disable-next-line react-hooks/set-state-in-effect
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

  function openEnumerate() {
    if (!boards) return
    if (selected.size === 0) return
    setEnumInc('1')
    setEnumOpen(true)
  }

  function closeEnumerate() {
    setEnumOpen(false)
  }

  function applyEnumerate() {
    if (!boards) return
    const inc = clampInt(enumInc, 1, 127)

    const ordered = selectedOrder.filter((id) => selected.has(id)).map(parseCellId).filter(Boolean) as Array<{
      board: 'Board0' | 'Board1'
      idx: number
    }>

    if (ordered.length === 0) {
      closeEnumerate()
      return
    }

    const baseCell = boards[ordered[0].board]?.[ordered[0].idx]
    if (!baseCell) {
      closeEnumerate()
      return
    }
    const baseNote = baseCell.note | 0

    const next: Boards = {
      Board0: boards.Board0.map((c) => ({ ...c })),
      Board1: boards.Board1.map((c) => ({ ...c })),
    }

    for (let k = 0; k < ordered.length; k++) {
      const { board, idx } = ordered[k]
      const cell = next[board][idx]
      if (!cell) continue
      const n = clampInt(String(baseNote + k * inc), 0, 127)
      cell.note = n
    }

    setBoards(next)
    pushPreview(next)
    closeEnumerate()
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

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brandTitle">XenWooting configurator</div>
        </div>

        <div className="controls">
          <label className="field">
            <span className="fieldLabel">Display</span>
            <select
              className="select"
              value={displayMode}
              onChange={(e) => setDisplayMode(e.target.value as 'label' | 'number' | 'both')}
            >
              <option value="label">Label only</option>
              <option value="number">MIDI only</option>
              <option value="both">All</option>
            </select>
          </label>

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
        {enumOpen && (
          <div
            className="modalOverlay"
            onPointerDown={(e) => {
              if (e.target === e.currentTarget) closeEnumerate()
            }}
          >
            <div
              className="modal"
              role="dialog"
              aria-modal="true"
              aria-label="Enumerate notes"
              onPointerDown={(e) => e.stopPropagation()}
            >
              <div className="modalTitle">Enumerate</div>
              <div className="modalSub">
                Leave first selected note as-is; assign following notes by increment.
              </div>
              <label className="field">
                <span className="fieldLabel">Increment (min 1)</span>
                <input
                  ref={enumInputRef}
                  className="input"
                  inputMode="numeric"
                  value={enumInc}
                  onChange={(e) => setEnumInc(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === 'Escape') {
                      e.preventDefault()
                      closeEnumerate()
                    }
                    if (e.key === 'Enter') {
                      e.preventDefault()
                      applyEnumerate()
                    }
                  }}
                />
              </label>
              <div className="row">
                <button className="btnSecondary" type="button" onClick={closeEnumerate}>
                  Abort
                </button>
                <button className="btn" type="button" onClick={applyEnumerate}>
                  Apply
                </button>
              </div>
            </div>
          </div>
        )}

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
                onClick={openEnumerate}
                disabled={!boards || selected.size === 0}
              >
                Enumerate
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
                    <span className="cellVal cellNoteName">{x.unicode}</span>
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
              <KeyboardView
                title="Board0"
                boardId="Board0"
                geometry={geometry}
                cells={boards.Board0}
                rotate180
                xOffsetU={3}
                noteNamesByIdx={noteNamesByBoard?.Board0}
                displayMode={displayMode}
                onHoverKey={(board, idx) => setHoveredKey({ board, idx })}
                onHoverEnd={() => setHoveredKey(null)}
                selected={selected}
                selectedOrder={selectedOrder}
                setSelected={setSelected}
                setSelectedOrder={setSelectedOrder}
                lastSelected={lastSelected}
                setLastSelected={setLastSelected}
                onKeyHighlight={onKeyHighlight}
              />
              <KeyboardView
                title="Board1"
                boardId="Board1"
                geometry={geometry}
                cells={boards.Board1}
                noteNamesByIdx={noteNamesByBoard?.Board1}
                displayMode={displayMode}
                onHoverKey={(board, idx) => setHoveredKey({ board, idx })}
                onHoverEnd={() => setHoveredKey(null)}
                selected={selected}
                selectedOrder={selectedOrder}
                setSelected={setSelected}
                setSelectedOrder={setSelectedOrder}
                lastSelected={lastSelected}
                setLastSelected={setLastSelected}
                onKeyHighlight={onKeyHighlight}
              />
            </>
          )}
        </section>
      </main>
    </div>
  )
}

export default App

function clampInt(s: string, min: number, max: number) {
  const n = Number.parseInt(s, 10)
  if (!Number.isFinite(n)) return min
  return Math.min(max, Math.max(min, n))
}

function parseCellId(id: string): { board: 'Board0' | 'Board1'; idx: number } | null {
  const [b, i] = id.split(':')
  if (b !== 'Board0' && b !== 'Board1') return null
  const idx = Number.parseInt(i || '', 10)
  if (!Number.isFinite(idx) || idx < 0 || idx >= 56) return null
  return { board: b, idx }
}

function visibleWtnIdxs(geometry: Geometry, rotate180: boolean): number[] {
  const keys = geometry.keys

  let minColByRow = [0, 0, 0, 0]
  if (rotate180) {
    const min = [255, 255, 255, 255]
    for (const k of keys) {
      const rr = 3 - k.row
      const cc = 13 - k.col
      min[rr] = Math.min(min[rr], cc)
    }
    for (let i = 0; i < 4; i++) {
      if (min[i] === 255) min[i] = 0
    }
    minColByRow = min
  }

  const out: number[] = []
  const seen = new Set<number>()
  for (const k of keys) {
    const rr = rotate180 ? 3 - k.row : k.row
    const cc0 = rotate180 ? 13 - k.col : k.col
    const cc = cc0 - (minColByRow[rr] || 0)
    const idx = rr * 14 + cc
    if (idx < 0 || idx >= 56) continue
    if (seen.has(idx)) continue
    seen.add(idx)
    out.push(idx)
  }
  return out
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
