import './App.css'
import { useEffect, useMemo, useState } from 'react'
import { fetchGeometry, fetchLayout, fetchLayouts, saveLayout } from './api'
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
  }

  async function onSave() {
    if (!boards) return
    setStatus('Saving...')
    try {
      const r = await saveLayout(layoutId, boards)
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

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brandTitle">XenWooting configurator</div>
        </div>

        <div className="controls">
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
