import { useEffect, useMemo, useRef, useState } from 'react'
import type { Cell, Geometry } from './types'

type KeyboardViewProps = {
  title: string
  boardId: 'Board0' | 'Board1'
  geometry: Geometry
  cells: Cell[]
  rotate180?: boolean
  xOffsetU?: number
  noteNamesByIdx?: Map<number, string>
  displayMode: 'label' | 'number' | 'both'
  overlayByIdx?: Map<number, { note: number; chan: number; col: string }>
  placementActive?: boolean
  onHoverKey?: (board: 'Board0' | 'Board1', idx: number) => void
  onHoverEnd?: () => void
  selected: Set<string>
  selectedOrder: string[]
  setSelected: (next: Set<string>) => void
  setSelectedOrder: (next: string[]) => void
  lastSelected: string | null
  setLastSelected: (id: string | null) => void
  onKeyHighlight?: (board: 'Board0' | 'Board1', idx: number, down: boolean) => void
}

export function KeyboardView({
  title,
  boardId,
  geometry,
  cells,
  rotate180,
  xOffsetU,
  noteNamesByIdx,
  displayMode,
  overlayByIdx,
  placementActive,
  onHoverKey,
  onHoverEnd,
  selected,
  selectedOrder,
  setSelected,
  setSelectedOrder,
  lastSelected,
  setLastSelected,
  onKeyHighlight,
}: KeyboardViewProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [scale, setScale] = useState(1)
  const [isMulti, setIsMulti] = useState(false)
  const [isRange, setIsRange] = useState(false)

  const keys = geometry.keys
  const gapPx = 2
  const xOff = xOffsetU || 0

  const minColByRow = useMemo(() => {
    if (!rotate180) return [0, 0, 0, 0]
    const min = [255, 255, 255, 255]
    for (const k of keys) {
      const rr = 3 - k.row
      const cc = 13 - k.col
      min[rr] = Math.min(min[rr], cc)
    }
    for (let i = 0; i < 4; i++) {
      if (min[i] === 255) min[i] = 0
    }
    return min
  }, [keys, rotate180])

  const keysWithIdx = useMemo(() => {
    return keys.map((k) => {
      const rr = rotate180 ? 3 - k.row : k.row
      const cc0 = rotate180 ? 13 - k.col : k.col
      const cc = cc0 - (minColByRow[rr] || 0)
      const wtnIdx = rr * 14 + cc
      return { k, wtnIdx }
    })
  }, [keys, rotate180, minColByRow])

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Shift') setIsRange(true)
      if (e.key === 'Control' || e.key === 'Meta' || e.key === 'Alt') setIsMulti(true)
    }
    const onKeyUp = (e: KeyboardEvent) => {
      if (e.key === 'Shift') setIsRange(false)
      if (e.key === 'Control' || e.key === 'Meta' || e.key === 'Alt') setIsMulti(false)
    }
    window.addEventListener('keydown', onKeyDown)
    window.addEventListener('keyup', onKeyUp)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      window.removeEventListener('keyup', onKeyUp)
    }
  }, [])

  useEffect(() => {
    const update = () => {
      const el = containerRef.current
      if (!el) return
      const w = el.getBoundingClientRect().width
      if (!w) return
      setScale(Math.min(w / (geometry.width + xOff), 120))
    }
    update()
    window.addEventListener('resize', update)
    return () => window.removeEventListener('resize', update)
  }, [geometry.width, xOff])

  const byIdx = useMemo(() => {
    const m = new Map<number, Cell>()
    for (let i = 0; i < cells.length; i++) m.set(i, cells[i])
    return m
  }, [cells])

  const keysByIdx = useMemo(() => {
    const m = new Map<number, (typeof keys)[number]>()
    for (const { k, wtnIdx } of keysWithIdx) m.set(wtnIdx, k)
    return m
  }, [keysWithIdx])

  function toggle(idx: number) {
    const id = `${boardId}:${idx}`

    // Range: only within same board.
    if (isRange && lastSelected && lastSelected.startsWith(`${boardId}:`)) {
      const lastIdx = Number.parseInt(lastSelected.split(':')[1] || '', 10)
      if (Number.isFinite(lastIdx)) {
        const lo = Math.min(lastIdx, idx)
        const hi = Math.max(lastIdx, idx)
        const next = new Set(selected)
        const nextOrder = [...selectedOrder]
        for (let i = lo; i <= hi; i++) {
          if (!keysByIdx.has(i)) continue
          const rid = `${boardId}:${i}`
          if (!next.has(rid)) {
            next.add(rid)
            nextOrder.push(rid)
          }
        }
        setSelected(next)
        setSelectedOrder(dedupeKeepOrder(nextOrder))
        setLastSelected(id)
        return
      }
    }

    if (isMulti) {
      const next = new Set(selected)
      const nextOrder = [...selectedOrder]
      if (next.has(id)) next.delete(id)
      else {
        next.add(id)
        nextOrder.push(id)
      }
      setSelected(next)
      setSelectedOrder(next.has(id) ? dedupeKeepOrder(nextOrder) : nextOrder.filter((x) => x !== id))
      setLastSelected(id)
      return
    }

    if (selected.size === 1 && selected.has(id)) {
      setSelected(new Set())
      setSelectedOrder([])
      setLastSelected(null)
      return
    }

    setSelected(new Set([id]))
    setSelectedOrder([id])
    setLastSelected(id)
  }

  return (
    <section className="kbd">
      <header className="kbdHeader">
        <div className="kbdTitle">{title}</div>
        <div className="kbdHeaderRight">
          <div className="kbdMeta">{keys.length} keys</div>
        </div>
      </header>
      <div className="kbdWrap" ref={containerRef}>
        <div
          className="kbdCanvas"
          style={{ width: (geometry.width + xOff) * scale, height: geometry.height * scale }}
        >
          {keysWithIdx.map(({ k, wtnIdx }) => {
            const cell = byIdx.get(wtnIdx)
            const id = `${boardId}:${wtnIdx}`
            const isSel = selected.has(id)
            const overlay = overlayByIdx?.get(wtnIdx)
            const rgb = overlay ? `#${overlay.col}` : cell ? `#${cell.col}` : '#444444'
            const name = noteNamesByIdx?.get(wtnIdx) || ''
            const showLabel = displayMode === 'label'
            const showNumber = displayMode === 'number'
            const showChan = displayMode === 'number'

            const x0 = rotate180 ? geometry.width - (k.x + k.w) : k.x
            const y0 = rotate180 ? geometry.height - (k.y + k.h) : k.y

            const left = (x0 + xOff) * scale + gapPx / 2
            const top = y0 * scale + gapPx / 2
            const width = Math.max(1, k.w * scale - gapPx)
            const height = Math.max(1, k.h * scale - gapPx)

            return (
              <button
                key={id}
                type="button"
                className={`key ${isSel ? 'keySelected' : ''} ${displayMode !== 'both' ? 'keySoloMode' : ''} ${displayMode === 'both' ? 'keyAllMode' : ''} ${overlay ? 'keyOverlay' : ''}`}
                style={{
                  left,
                  top,
                  width,
                  height,
                  backgroundColor: rgb,
                }}
                onPointerDown={(e) => {
                  try {
                    // Keep getting pointer events even if pointer leaves the key.
                    ;(e.currentTarget as HTMLButtonElement).setPointerCapture(e.pointerId)
                  } catch {
                    // ignore
                  }
                  if (!placementActive) onKeyHighlight?.(boardId, wtnIdx, true)
                }}
                onPointerUp={() => {
                  if (!placementActive) onKeyHighlight?.(boardId, wtnIdx, false)
                }}
                onPointerCancel={() => {
                  if (!placementActive) onKeyHighlight?.(boardId, wtnIdx, false)
                }}
                onPointerEnter={() => onHoverKey?.(boardId, wtnIdx)}
                onPointerLeave={() => {
                  if (!placementActive) onKeyHighlight?.(boardId, wtnIdx, false)
                  onHoverEnd?.()
                }}
                onClick={() => toggle(wtnIdx)}
                title={`${boardId} idx ${wtnIdx}`}
              >
                {cell && showChan && (
                  <div className="keyTop">
                    <span className="keyChan">[{overlay ? overlay.chan : cell.chan}]</span>
                  </div>
                )}
                <div className="keyInner">
                  {cell && displayMode === 'both' && (
                    <>
                      <div className="keyNote">{(overlay ? overlay.chan : cell.chan)}-{(overlay ? overlay.note : cell.note)}</div>
                      {name && <div className="keyName">{name}</div>}
                    </>
                  )}
                  {cell && showNumber && <div className="keySolo">{overlay ? overlay.note : cell.note}</div>}
                  {cell && showLabel && name && <div className="keySolo keySoloLabel">{name}</div>}
                </div>
              </button>
            )
          })}
        </div>
      </div>
    </section>
  )
}

function dedupeKeepOrder(ids: string[]) {
  const out: string[] = []
  const seen = new Set<string>()
  for (const id of ids) {
    if (seen.has(id)) continue
    seen.add(id)
    out.push(id)
  }
  return out
}
