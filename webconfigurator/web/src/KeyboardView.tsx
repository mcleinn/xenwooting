import { useEffect, useMemo, useRef, useState } from 'react'
import type { Cell, Geometry } from './types'

type KeyboardViewProps = {
  title: string
  boardId: 'Board0' | 'Board1'
  geometry: Geometry
  cells: Cell[]
  rotate180?: boolean
  xOffsetU?: number
  selected: Set<string>
  selectedOrder: string[]
  setSelected: (next: Set<string>) => void
  setSelectedOrder: (next: string[]) => void
  lastSelected: string | null
  setLastSelected: (id: string | null) => void
}

export function KeyboardView({
  title,
  boardId,
  geometry,
  cells,
  rotate180,
  xOffsetU,
  selected,
  selectedOrder,
  setSelected,
  setSelectedOrder,
  lastSelected,
  setLastSelected,
}: KeyboardViewProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [scale, setScale] = useState(1)
  const [isMulti, setIsMulti] = useState(false)
  const [isRange, setIsRange] = useState(false)

  const keys = geometry.keys
  const gapPx = 2
  const xOff = xOffsetU || 0

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
    for (const k of keys) m.set(k.idx, k)
    return m
  }, [keys])

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
        <div className="kbdMeta">{keys.length} keys</div>
      </header>
      <div className="kbdWrap" ref={containerRef}>
        <div
          className="kbdCanvas"
          style={{ width: (geometry.width + xOff) * scale, height: geometry.height * scale }}
        >
          {keys.map((k) => {
            const cell = byIdx.get(k.idx)
            const id = `${boardId}:${k.idx}`
            const isSel = selected.has(id)
            const rgb = cell ? `#${cell.col}` : '#444444'

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
                className={`key ${isSel ? 'keySelected' : ''}`}
                style={{
                  left,
                  top,
                  width,
                  height,
                  backgroundColor: rgb,
                }}
                onClick={() => toggle(k.idx)}
                title={`${boardId} idx ${k.idx}`}
              >
                <div className="keyTop">
                  {cell && <span className="keyChan">[{cell.chan}]</span>}
                </div>
                {cell && <div className="keyNote">{cell.note}</div>}
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
