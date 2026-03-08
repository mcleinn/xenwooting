import { useEffect, useMemo, useState } from 'react'
import './GuidePage.css'
import { fetchChordCatalogue, fetchLayoutIsomorphic, fetchLayouts, guideStart, guideStop } from './api'

type LayoutInfo = { id: string; name: string; wtnPath: string }

type IsoInfo = {
  layoutId: string
  ok: boolean
  edo: number
  dq: number | null
  dr: number | null
  axis3: number | null
  reason: string | null
}

type CatalogueItem = { pcsRoot: number[]; pattern: string; bestName: string; allNames: string[] }

type LiveState = {
  seq?: number
  layout?: { id?: string }
  mode?: { guide_mode?: string; guide_chord_key?: string | null; guide_root_pc?: number | null; screensaver_active?: boolean }
  pressed?: { Board0?: number[]; Board1?: number[] }
}

type Axial = { q: number; r: number }
type Shape = {
  pts: Axial[]
  pcByPt: Map<string, number>
  score: { maxDist: number; sumDist: number; area: number }
}

type ShapeLibrary = Map<string, Shape[]> // key: `${pcsKey}::${absSigKey}`

function mod(n: number, m: number) {
  const x = n % m
  return x < 0 ? x + m : x
}



function ptKey(p: Axial) {
  return `${p.q},${p.r}`
}

function axialToDoubled(p: Axial): { x: number; y: number } {
  // x=r, y=r+2q
  return { x: p.r, y: p.r + 2 * p.q }
}

function neighborsAxial(p: Axial): Axial[] {
  // Corresponds to doubled-y neighbors.
  // (x,y) deltas: (0,+2)->(q+1,r), (+1,+1)->(q,r+1), (+1,-1)->(q-1,r+1), and opposites.
  return [
    { q: p.q + 1, r: p.r },
    { q: p.q - 1, r: p.r },
    { q: p.q, r: p.r + 1 },
    { q: p.q, r: p.r - 1 },
    { q: p.q - 1, r: p.r + 1 },
    { q: p.q + 1, r: p.r - 1 },
  ]
}

function hexDist(a: Axial, b: Axial) {
  // axial (q,r) -> cube (x=q, y=r, z=-q-r)
  const dq = a.q - b.q
  const dr = a.r - b.r
  const ds = -a.q - a.r - (-b.q - b.r)
  return Math.max(Math.abs(dq), Math.abs(dr), Math.abs(ds))
}

function excerptNodesForShortestPaths(pressed: Axial[]) {
  const uniq = new Map<string, Axial>()
  for (const p of pressed) uniq.set(ptKey(p), p)
  const pts = Array.from(uniq.values())
  if (pts.length === 0) return new Set<string>()

  // For a single point, show just its immediate neighborhood for context.
  if (pts.length === 1) {
    const s = new Set<string>()
    s.add(ptKey(pts[0]!))
    for (const nb of neighborsAxial(pts[0]!)) s.add(ptKey(nb))
    return s
  }

  const out = new Set<string>()
  for (const p of pts) out.add(ptKey(p))

  for (let i = 0; i < pts.length; i++) {
    for (let j = i + 1; j < pts.length; j++) {
      const a = pts[i]!
      const b = pts[j]!
      const D = hexDist(a, b)
      // Enumerate nodes within distance D of a and keep those on any shortest path.
      const around = pointsInRadius(D)
      for (const d0 of around) {
        const n = { q: a.q + d0.q, r: a.r + d0.r }
        if (hexDist(a, n) + hexDist(n, b) === D) out.add(ptKey(n))
      }
    }
  }

  return out
}

function pointsInRadius(R: number): Axial[] {
  const out: Axial[] = []
  for (let q = -R; q <= R; q++) {
    const r1 = Math.max(-R, -q - R)
    const r2 = Math.min(R, -q + R)
    for (let r = r1; r <= r2; r++) out.push({ q, r })
  }
  return out
}

function uniqSorted(nums: number[]) {
  const s = new Set<number>()
  for (const n of nums) s.add(n)
  return Array.from(s.values()).sort((a, b) => a - b)
}

function pcAt(p: Axial, edo: number, dq: number, dr: number) {
  return mod(p.q * dq + p.r * dr, edo)
}

function buildLibrary(edo: number, dq: number, dr: number, tones: 3 | 4, R: number, K: number): ShapeLibrary {
  const pts = pointsInRadius(R)
  const originIdx = pts.findIndex((p) => p.q === 0 && p.r === 0)
  const origin = originIdx >= 0 ? pts[originIdx] : { q: 0, r: 0 }
  const others = pts.filter((p) => !(p.q === 0 && p.r === 0))

  const bestByKey: ShapeLibrary = new Map()

  const consider = (shapePts: Axial[]) => {
    // Canonicalize so the lowest absolute pitch is at the origin.
    const abs = shapePts.map((p) => p.q * dq + p.r * dr)
    let minAbs = Infinity
    let minIdx = -1
    for (let i = 0; i < abs.length; i++) {
      const v = abs[i]!
      if (v < minAbs) {
        minAbs = v
        minIdx = i
      }
    }
    if (minIdx < 0) return

    const bass = shapePts[minIdx]!
    const normPts = shapePts.map((p) => ({ q: p.q - bass.q, r: p.r - bass.r }))
    // Canonical ordering so equivalent patterns don't appear as duplicates.
    normPts.sort((a, b) => {
      const ao = a.q === 0 && a.r === 0
      const bo = b.q === 0 && b.r === 0
      if (ao && !bo) return -1
      if (!ao && bo) return 1
      if (a.q !== b.q) return a.q - b.q
      return a.r - b.r
    })
    const absNorm = abs.map((x) => x - minAbs)

    // Option-B voicing constraint: all chord tones are within one octave above the bass.
    const maxAbs = Math.max(...absNorm)
    if (!Number.isFinite(maxAbs) || maxAbs >= edo) return
    if (absNorm.some((x) => x < 0)) return

    const pcs = uniqSorted(absNorm.map((x) => mod(x, edo)))
    if (pcs.length !== normPts.length) return
    if (pcs[0] !== 0) return

    const pcsKey = pcs.join(',')
    const absSigKey = [...absNorm].sort((a, b) => a - b).join(',')
    const key = `${pcsKey}::${absSigKey}`

    let maxDist = 0
    let sumDist = 0
    for (let i = 0; i < normPts.length; i++) {
      for (let j = i + 1; j < normPts.length; j++) {
        const d = hexDist(normPts[i]!, normPts[j]!)
        maxDist = Math.max(maxDist, d)
        sumDist += d
      }
    }

    let minX = Infinity
    let maxX = -Infinity
    let minY = Infinity
    let maxY = -Infinity
    for (const p of normPts) {
      const d = axialToDoubled(p)
      minX = Math.min(minX, d.x)
      maxX = Math.max(maxX, d.x)
      minY = Math.min(minY, d.y)
      maxY = Math.max(maxY, d.y)
    }
    const area = (maxX - minX) * (maxY - minY)

    const pcByPt = new Map<string, number>()
    for (const p of normPts) pcByPt.set(ptKey(p), pcAt(p, edo, dq, dr))

    const shape: Shape = { pts: normPts, pcByPt, score: { maxDist, sumDist, area } }

    const arr = bestByKey.get(key) || []
    const ptsKey = normPts.map((p) => ptKey(p)).join('|')
    if (arr.some((s) => s.pts.map((p) => ptKey(p)).join('|') === ptsKey)) return
    arr.push(shape)
    arr.sort((a, b) => {
      if (a.score.maxDist !== b.score.maxDist) return a.score.maxDist - b.score.maxDist
      if (a.score.sumDist !== b.score.sumDist) return a.score.sumDist - b.score.sumDist
      if (a.score.area !== b.score.area) return a.score.area - b.score.area
      return a.pts.length - b.pts.length
    })
    if (arr.length > K) arr.length = K
    bestByKey.set(key, arr)
  }

  if (tones === 3) {
    for (let i = 0; i < others.length; i++) {
      for (let j = i + 1; j < others.length; j++) {
        consider([origin, others[i]!, others[j]!])
      }
    }
  } else {
    for (let i = 0; i < others.length; i++) {
      for (let j = i + 1; j < others.length; j++) {
        for (let k = j + 1; k < others.length; k++) {
          consider([origin, others[i]!, others[j]!, others[k]!])
        }
      }
    }
  }

  return bestByKey
}

function project12(edo: number, semis: number[]) {
  const pcs = uniqSorted(
    semis
      .map((s) => Math.round((s * edo) / 12))
      .map((n) => mod(n, edo)),
  )
  return pcs
}

function inversions(edo: number, pcsRoot: number[]) {
  const out: Array<{ bass: number; pcsInv: number[]; degreeByPc: Map<number, number> }> = []
  for (let i = 0; i < pcsRoot.length; i++) {
    const bass = pcsRoot[i]!
    const degreeByPc = new Map<number, number>()
    for (let k = 0; k < pcsRoot.length; k++) {
      const invPc = mod(pcsRoot[k]! - bass, edo)
      degreeByPc.set(invPc, k)
    }
    const pcsInv = uniqSorted(pcsRoot.map((pc) => mod(pc - bass, edo)))
    out.push({ bass, pcsInv, degreeByPc })
  }
  return out
}

function parseQuery() {
  const q = new URLSearchParams(window.location.search)
  const layout = String(q.get('layout') || '')
  return { layout }
}

function setQuery(next: { layout?: string }) {
  const q = new URLSearchParams(window.location.search)
  if (typeof next.layout === 'string') {
    if (next.layout) q.set('layout', next.layout)
    else q.delete('layout')
  }
  const url = `${window.location.pathname}?${q.toString()}`.replace(/\?$/, '')
  window.history.replaceState({}, '', url)
}

function chordKey(edo: number, title: string, pcsRoot: number[]) {
  return `${edo}|${String(title || '').trim()}|${(pcsRoot || []).join(',')}`
}

function collectOpenChordKeysFromDom() {
  const els = Array.from(document.querySelectorAll('details.chAcc[open]')) as Array<HTMLElement>
  const keys: string[] = []
  for (const el of els) {
    const k = el.dataset?.chordKey
    if (typeof k === 'string' && k) keys.push(k)
  }
  return keys
}

function TuningChordDetails({
  chordKey,
  summary,
  summaryTooltip,
  headerRight,
  children,
}: {
  chordKey: string
  summary: string
  summaryTooltip: string
  headerRight: React.ReactNode
  children: React.ReactNode
}) {
  const [open, setOpen] = useState(false)
  return (
    <details
      className="chAcc"
      data-chord-key={chordKey}
      onToggle={(e) => {
        setOpen((e.currentTarget as HTMLDetailsElement).open)
      }}
    >
      <summary className="chSum">
        <span className="chSumTitle" title={summaryTooltip}>{summary}</span>
        <span className="chSumRight">{headerRight}</span>
      </summary>
      {open ? <div className="chBody">{children}</div> : null}
    </details>
  )
}

function HexExcerptSvg({
  degreeByPc,
  shape,
}: {
  degreeByPc: Map<number, number>
  shape: Shape
}) {
  const pressed = shape.pts

  const pressedByXY = new Map<string, { x: number; y: number; pc: number }>()
  for (const p of pressed) {
    const d = axialToDoubled(p)
    const pc = shape.pcByPt.get(ptKey(p)) ?? 0
    pressedByXY.set(xyKey(d.x, d.y), { x: d.x, y: d.y, pc })
  }

  // Excerpt: only the nodes that lie on shortest paths between pressed nodes.
  const excerpt = new Map<string, { x: number; y: number }>()
  const exKeys = excerptNodesForShortestPaths(pressed)
  for (const k of exKeys.values()) {
    const [qs, rs] = k.split(',')
    const q = Number.parseInt(qs || '', 10)
    const r = Number.parseInt(rs || '', 10)
    if (!Number.isFinite(q) || !Number.isFinite(r)) continue
    const dd = axialToDoubled({ q, r })
    excerpt.set(xyKey(dd.x, dd.y), { x: dd.x, y: dd.y })
  }

  // Build bounds.
  let minX = Infinity
  let maxX = -Infinity
  let minY = Infinity
  let maxY = -Infinity
  for (const p of excerpt.values()) {
    minX = Math.min(minX, p.x)
    maxX = Math.max(maxX, p.x)
    minY = Math.min(minY, p.y)
    maxY = Math.max(maxY, p.y)
  }

  const hx = 22
  const hy = 18
  // Keep margins tight so cards don't look "bottom heavy".
  const pad = 16
  const w = Math.max(1, (maxY - minY) * hx + pad * 2)
  const h = Math.max(1, (maxX - minX) * hy + pad * 2)

  const coordsByXY = new Map<string, { cx: number; cy: number; x: number; y: number }>()
  for (const p of excerpt.values()) {
    const cx = pad + (p.y - minY) * hx
    const cy = pad + (p.x - minX) * hy
    coordsByXY.set(xyKey(p.x, p.y), { cx, cy, x: p.x, y: p.y })
  }

  const edges: Array<{ x1: number; y1: number; x2: number; y2: number }> = []
  for (const p of excerpt.values()) {
    for (const [dx, dy] of [
      [0, 2],
      [1, 1],
      [1, -1],
    ]) {
      const a = coordsByXY.get(xyKey(p.x, p.y))
      const b = coordsByXY.get(xyKey(p.x + dx, p.y + dy))
      if (!a || !b) continue
      edges.push({ x1: a.cx, y1: a.cy, x2: b.cx, y2: b.cy })
    }
  }

  const palette = ['#F59E0B', '#34D399', '#60A5FA', '#F87171']
  const pressedXY = new Set(pressedByXY.keys())

  // Bass is pc=0 in this inversion space; origin is always included.
  const bassXY = (() => {
    const origin = axialToDoubled({ q: 0, r: 0 })
    return xyKey(origin.x, origin.y)
  })()

  return (
    <svg className="hexSvg" viewBox={`0 0 ${w} ${h}`} role="img" aria-label="pattern">
      {edges.map((e, i) => (
        <line key={i} className="hexEdge" x1={e.x1} y1={e.y1} x2={e.x2} y2={e.y2} />
      ))}

      {Array.from(coordsByXY.entries()).map(([xy, c]) => {
        const pressedInfo = pressedByXY.get(xy)
        const isPressed = pressedInfo !== undefined
        let fill = ''
        let stroke = ''
        if (isPressed) {
          const pc = pressedInfo!.pc
          const deg = degreeByPc.get(pc) ?? 0
          const col = palette[Math.min(palette.length - 1, Math.max(0, deg))] || '#FFFFFF'
          fill = col
          stroke = col
        }
        const isBass = xy === bassXY
        return (
          <g key={xy} transform={`translate(${c.cx},${c.cy})`}>
            <circle
              className={isPressed ? 'hexDot hexDotPressed' : 'hexDot'}
              r={9}
              style={
                isPressed
                  ? ({
                      opacity: 0.92,
                      // Use CSS variables so print styles can override defaults.
                      '--dot-fill': fill,
                      '--dot-stroke': stroke,
                    } as unknown as React.CSSProperties)
                  : undefined
              }
            />
            {isBass && pressedXY.has(xy) ? <circle className="hexRing" r={12.5} /> : null}
          </g>
        )
      })}
    </svg>
  )
}

function InversionPatterns({
  edo,
  dq,
  dr,
  title,
  pcsRoot,
  inversionIndex,
  inv,
  shapes,
  expandAll,
}: {
  edo: number
  dq: number
  dr: number
  title: string
  pcsRoot: number[]
  inversionIndex: number
  inv: { bass: number; pcsInv: number[]; degreeByPc: Map<number, number> }
  shapes: Shape[]
  expandAll: boolean
}) {
  const [showAll, setShowAll] = useState(false)
  const limit = 2
  const effectiveAll = expandAll || showAll
  const shown = effectiveAll ? shapes : shapes.slice(0, limit)
  const more = Math.max(0, shapes.length - shown.length)

  return (
    <>
      <div className="patRow">
        {shown.map((s, k) => (
          <div
            key={k}
            className="patCard"
            role="button"
            tabIndex={0}
            title="Copy fingering JSON to clipboard"
            onClick={(e) => {
              e.preventDefault()
              e.stopPropagation()
              const payload = {
                edo,
                dq,
                dr,
                chordTitle: title,
                pcsRoot,
                inversionIndex,
                pcsInv: inv.pcsInv,
                fingering: {
                  axial: s.pts.map((p) => ({ q: p.q, r: p.r })),
                  doubled: s.pts.map((p) => {
                    const d = axialToDoubled(p)
                    return { x: d.x, y: d.y }
                  }),
                  pcsAtPts: s.pts.map((p) => {
                    const pc = s.pcByPt.get(ptKey(p)) ?? 0
                    const degree = inv.degreeByPc.get(pc) ?? 0
                    return { q: p.q, r: p.r, pc, degree }
                  }),
                  absSteps: s.pts.map((p) => p.q * dq + p.r * dr),
                },
              }
              const text = JSON.stringify(payload)
              void copyTextToClipboard(text).then((ok) => {
                if (ok) return
                window.prompt('Copy fingering JSON:', text)
              })
            }}
            onKeyDown={(e) => {
              if (e.key !== 'Enter' && e.key !== ' ') return
              e.preventDefault()
              ;(e.currentTarget as HTMLDivElement).click()
            }}
          >
            <HexExcerptSvg degreeByPc={inv.degreeByPc} shape={s} />
          </div>
        ))}
        {shapes.length === 0 ? (
          <div className="empty">No same-voicing patterns found (try expanding the search radius).</div>
        ) : null}
      </div>

      {!expandAll && more > 0 ? (
        <div className="invMore">
          <button className="gBtnSecondary" type="button" onClick={() => setShowAll(true)}>
            Show {more} more
          </button>
        </div>
      ) : null}
    </>
  )
}

function PrintChordPage({
  edo,
  title,
  pcsRoot,
  lib,
}: {
  edo: number
  title: string
  pcsRoot: number[]
  lib: ShapeLibrary
}) {
  const invs = inversions(edo, pcsRoot)
  const invLabels = invs.map((_, i) => (i === 0 ? 'Root' : i === 1 ? '1st' : i === 2 ? '2nd' : i === 3 ? '3rd' : `${i}th`))

  const shapesByInv = invs.map((inv) => {
    const pcsKey = inv.pcsInv.join(',')
    const absSigKey = inv.pcsInv.join(',')
    const shapes = lib.get(`${pcsKey}::${absSigKey}`) || []
    return shapes.slice(0, 2)
  })

  return (
    <section className="printChordPage">
      <div className="printChordTitle">
        {title}
        <sup className="printTuningSup">{edo}-EDO</sup>
      </div>

      <div className="printInvGrid" style={{ gridTemplateColumns: `repeat(${invs.length}, 1fr)` }}>
        {invs.map((inv, invIdx) => (
          <div key={`col:${invIdx}`} className="printInvCol">
            <div className="printInvHead">{invLabels[invIdx]}</div>

            <div className="printSlot">
              {shapesByInv[invIdx]![0] ? <HexExcerptSvg degreeByPc={inv.degreeByPc} shape={shapesByInv[invIdx]![0]!} /> : null}
            </div>
            <div className="printSlot">
              {shapesByInv[invIdx]![1] ? <HexExcerptSvg degreeByPc={inv.degreeByPc} shape={shapesByInv[invIdx]![1]!} /> : null}
            </div>
          </div>
        ))}
      </div>
    </section>
  )
}

function xyKey(x: number, y: number) {
  return `${x},${y}`
}

export default function GuidePage() {
  const initial = useMemo(() => parseQuery(), [])
  const [layouts, setLayouts] = useState<LayoutInfo[]>([])
  const [layoutId, setLayoutId] = useState<string>(initial.layout)
  const [iso, setIso] = useState<IsoInfo | null>(null)
  const [isoStatus, setIsoStatus] = useState<'idle' | 'loading' | 'ready' | 'error'>('idle')
  const [catalogue, setCatalogue] = useState<CatalogueItem[]>([])
  const [expandAll, setExpandAll] = useState<boolean>(false)
  const [loadError, setLoadError] = useState<string>('')
  const [printChordKeys, setPrintChordKeys] = useState<string[]>([])

  const [live, setLive] = useState<LiveState | null>(null)
  const [guidePendingKey, setGuidePendingKey] = useState<string | null>(null)

  useEffect(() => {
    const onBeforePrint = () => {
      try {
        setPrintChordKeys(collectOpenChordKeysFromDom())
      } catch {
        setPrintChordKeys([])
      }
    }
    window.addEventListener('beforeprint', onBeforePrint)
    return () => window.removeEventListener('beforeprint', onBeforePrint)
  }, [])

  useEffect(() => {
    let cancelled = false
    fetchLayouts()
      .then((r) => {
        if (cancelled) return
        const ls = Array.isArray(r.layouts) ? (r.layouts as LayoutInfo[]) : []
        setLayouts(ls)
        if (!layoutId && ls.length) {
          setLayoutId(ls[0]!.id)
          setQuery({ layout: ls[0]!.id })
        }
      })
      .catch(() => {})
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  // Subscribe to live state for guide-mode status.
  useEffect(() => {
    let closed = false
    const es = new EventSource(new URL('api/live/stream', `${window.location.origin}/wtn/`).toString())
    es.addEventListener('state', (ev: MessageEvent) => {
      if (closed) return
      try {
        const obj: unknown = JSON.parse(String(ev.data || 'null'))
        if (!obj || typeof obj !== 'object') return
        setLive(obj as LiveState)
      } catch {
        // ignore
      }
    })
    es.onerror = () => {
      // silent retry
    }
    return () => {
      closed = true
      es.close()
    }
  }, [])

  useEffect(() => {
    if (!layoutId) return
    let cancelled = false
    setQuery({ layout: layoutId })
    setLoadError('')
    setIsoStatus('loading')
    fetchLayoutIsomorphic(layoutId)
      .then(async (isoInfo) => {
        if (!isoInfo || typeof isoInfo !== 'object') throw new Error('Invalid isomorphic response')
        const cat = isoInfo?.ok && isoInfo?.edo
          ? await fetchChordCatalogue(isoInfo.edo, { limit: 300, minTones: 3, maxTones: 4 })
          : { edo: isoInfo?.edo ?? 12, results: [] as CatalogueItem[] }
        if (cancelled) return
        setIso(isoInfo)
        setIsoStatus('ready')
        setCatalogue(Array.isArray(cat?.results) ? cat.results : [])
      })
      .catch((e) => {
        if (cancelled) return
        setIso({ layoutId, ok: false, edo: 12, dq: null, dr: null, axis3: null, reason: 'Failed to load isomorphic info from server' })
        setIsoStatus('error')
        setCatalogue([])
        setLoadError(e instanceof Error ? e.message : String(e))
      })
    return () => {
      cancelled = true
    }
  }, [layoutId])

  const edo = iso?.edo ?? 12
  const dq = iso?.dq ?? 0
  const dr = iso?.dr ?? 0

  const guideMode = String(live?.mode?.guide_mode || 'off')
  const guideChordKey = (live?.mode?.guide_chord_key ?? null) as string | null

  // Clear pending state once xenwooting reports guide mode.
  useEffect(() => {
    if (!guidePendingKey) return
    if (guideChordKey === guidePendingKey) {
      setGuidePendingKey(null)
      return
    }
    // Do not clear on `guideMode === 'off'` immediately; SSE can lag behind the
    // POST /guide/start and we want the button label to switch immediately.
  }, [guidePendingKey, guideChordKey, guideMode])

  const catByPcs = useMemo(() => {
    const m = new Map<string, CatalogueItem>()
    for (const it of catalogue) {
      const key = (it.pcsRoot || []).join(',')
      if (!key) continue
      if (!m.has(key)) m.set(key, it)
    }
    return m
  }, [catalogue])

  const libs = useMemo(() => {
    if (!iso?.ok || iso.dq === null || iso.dr === null) return null
    // Use a larger search radius but only display the first 2 patterns by default.
    // The library is keyed by (pitch classes + absolute-step signature), so we never
    // lose same-voicing patterns to unrelated octave-displaced shapes.
    const lib3 = buildLibrary(edo, dq, dr, 3, 6, 24)
    const lib4 = buildLibrary(edo, dq, dr, 4, 4, 18)
    return { lib3, lib4 }
  }, [iso?.ok, iso?.dq, iso?.dr, edo, dq, dr])

  const coreTriads = useMemo(() => {
    const core = [
      { id: 'maj', want: 'Major triad', semis: [0, 4, 7] },
      { id: 'min', want: 'Minor triad', semis: [0, 3, 7] },
      { id: 'dim', want: 'Diminished triad', semis: [0, 3, 6] },
      { id: 'aug', want: 'Augmented triad', semis: [0, 4, 8] },
      { id: 'sus2', want: 'Sus2', semis: [0, 2, 7] },
      { id: 'sus4', want: 'Sus4', semis: [0, 5, 7] },
    ]
    const out: Array<{ id: string; pcsRoot: number[]; item: CatalogueItem }> = []
    for (const c of core) {
      const pcs = project12(edo, c.semis)
      if (pcs.length !== 3) continue
      const it = catByPcs.get(pcs.join(','))
      if (!it || !it.bestName) continue
      if (String(it.bestName).toLowerCase().includes('inversion')) continue
      out.push({ id: c.id, pcsRoot: pcs, item: it })
    }
    return out
  }, [edo, catByPcs])

  const coreSevenths = useMemo(() => {
    const core = [
      { id: '7', want: 'Dominant 7th', semis: [0, 4, 7, 10] },
      { id: 'maj7', want: 'Major 7th', semis: [0, 4, 7, 11] },
      { id: 'min7', want: 'Minor 7th', semis: [0, 3, 7, 10] },
      { id: 'hdim7', want: 'Half-diminished 7th', semis: [0, 3, 6, 10] },
      { id: 'dim7', want: 'Diminished 7th', semis: [0, 3, 6, 9] },
    ]
    const out: Array<{ id: string; pcsRoot: number[]; item: CatalogueItem }> = []
    for (const c of core) {
      const pcs = project12(edo, c.semis)
      if (pcs.length !== 4) continue
      const it = catByPcs.get(pcs.join(','))
      if (!it || !it.bestName) continue
      if (String(it.bestName).toLowerCase().includes('inversion')) continue
      out.push({ id: c.id, pcsRoot: pcs, item: it })
    }
    return out
  }, [edo, catByPcs])

  const tuningSpecific = useMemo(() => {
    const used = new Set<string>()
    for (const x of coreTriads) used.add(x.pcsRoot.join(','))
    for (const x of coreSevenths) used.add(x.pcsRoot.join(','))

    const candidates = catalogue
      .filter((it) => {
        const pcs = it.pcsRoot || []
        if (pcs.length < 3 || pcs.length > 4) return false
        const key = pcs.join(',')
        if (used.has(key)) return false
        return Boolean(it.bestName)
      })
      .filter((it) => !String(it.bestName || '').toLowerCase().includes('inversion'))
      // TuningSpecific should not include EDO12-derived names.
      .filter((it) => !String(it.bestName || '').toLowerCase().includes('-edo12'))
      // Also skip JI-approximated names with cents error suffix.
      .filter((it) => !String(it.bestName || '').toLowerCase().match(/~\d+c\b/))

    // Show all remaining chords (can be large; bodies render lazily).
    return candidates
  }, [catalogue, coreTriads, coreSevenths])

  const layoutName = useMemo(() => {
    const hit = layouts.find((l) => l.id === layoutId)
    return hit?.name || hit?.id || layoutId || 'Guide'
  }, [layouts, layoutId])

  const kbdAvailable = Boolean(layoutId)

  const activeChordKey = guidePendingKey || guideChordKey

  const headerButton = (title: string, pcsRoot: number[]) => {
    const ck = chordKey(edo, title, pcsRoot)
    const isThis = Boolean(activeChordKey && activeChordKey === ck && (guideMode !== 'off' || guidePendingKey))
    const isWaiting = Boolean(activeChordKey && activeChordKey === ck && (guideMode === 'wait_root' || guidePendingKey))

    return (
      <span className="chHdrBtnWrap">
        {isWaiting ? <span className="kbdHint">Waiting for root note</span> : null}
        <button
          className={isThis ? 'gBtnSecondary' : 'gBtn'}
          type="button"
          disabled={!kbdAvailable}
          onClick={(e) => {
            e.preventDefault()
            e.stopPropagation()
            if (isThis) {
              setGuidePendingKey(null)
              void guideStop().catch(() => {})
              return
            }
            setGuidePendingKey(ck)
            void guideStart(layoutId, ck, title, pcsRoot).catch(() => {
              setGuidePendingKey(null)
            })
          }}
        >
          {isThis ? 'Reset keyboard' : 'Show on keyboard'}
        </button>
      </span>
    )
  }

  return (
    <div className="guideRoot">
      <header className="guideTop">
        <div className="guideTitle">Tuning guide</div>
        <div className="guideControls">
          <label className="gField">
            <span className="gLabel">Layout</span>
            <select
              className="gSelect"
              value={layoutId}
              onChange={(e) => {
                const v = e.target.value
                setLayoutId(v)
              }}
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
            className="gBtn"
            type="button"
            onClick={() => setExpandAll((x) => !x)}
            disabled={!iso?.ok}
          >
            {expandAll ? 'Collapse all' : 'Expand all'}
          </button>

          <button
            className="gBtnSecondary"
            type="button"
            onClick={() => {
              if (!iso?.ok) return
              setPrintChordKeys(collectOpenChordKeysFromDom())
              setTimeout(() => window.print(), 50)
            }}
            disabled={!iso?.ok}
          >
            Print
          </button>
        </div>
      </header>

      <main className="guideMain">
        <div className="guideMeta">
          <div className="guideMetaTitle">{layoutName}</div>
          {iso?.ok ? (
            <div className="guideMetaRow axisRow">
              {formatAxisSteps(edo, dq, dr, iso.axis3 ?? dr - dq).map((s) => (
                <span key={s.dir} className={`axisBox axisBox--${s.dir}`}>
                  {s.text}
                </span>
              ))}
            </div>
          ) : isoStatus === 'loading' || isoStatus === 'idle' ? (
            <div className="guideMetaRow guideLoading">Checking tuning...</div>
          ) : (
            <div className="guideMetaRow guideBad">
              Guide unavailable: layout is not shared-isomorphic{iso?.reason ? ` (${iso.reason})` : ''}.
              {loadError ? ` (${loadError})` : ''}
            </div>
          )}
        </div>

        {iso?.ok && libs ? (
          <>
          <div className="guideSections">
            <details className="acc" open={expandAll}>
              <summary className="accSum">Core triads</summary>
              <div className="accBody">
                {coreTriads.map((c) => (
                  <details
                    key={c.id}
                    className="chAcc"
                    data-chord-key={chordKey(edo, c.item.bestName, c.pcsRoot)}
                    open={expandAll}
                  >
                    <summary className="chSum">
                      <span className="chSumTitle" title={otherNamesTooltip(c.item.bestName, c.item.allNames)}>{c.item.bestName}</span>
                      <span className="chSumRight">{headerButton(c.item.bestName, c.pcsRoot)}</span>
                    </summary>
                    <div className="chBody">
                      <ChordBlock
                        edo={edo}
                        dq={dq}
                        dr={dr}
                        title={c.item.bestName}
                        pcsRoot={c.pcsRoot}
                        lib={libs.lib3}
                        expandAll={expandAll}
                      />
                    </div>
                  </details>
                ))}
                {coreTriads.length === 0 ? <div className="empty">No core triads found for this EDO.</div> : null}
              </div>
            </details>

            <details className="acc" open={expandAll}>
              <summary className="accSum">Core sevenths</summary>
              <div className="accBody">
                {coreSevenths.map((c) => (
                  <details
                    key={c.id}
                    className="chAcc"
                    data-chord-key={chordKey(edo, c.item.bestName, c.pcsRoot)}
                    open={expandAll}
                  >
                    <summary className="chSum">
                      <span className="chSumTitle" title={otherNamesTooltip(c.item.bestName, c.item.allNames)}>{c.item.bestName}</span>
                      <span className="chSumRight">{headerButton(c.item.bestName, c.pcsRoot)}</span>
                    </summary>
                    <div className="chBody">
                      <ChordBlock
                        edo={edo}
                        dq={dq}
                        dr={dr}
                        title={c.item.bestName}
                        pcsRoot={c.pcsRoot}
                        lib={libs.lib4}
                        expandAll={expandAll}
                      />
                    </div>
                  </details>
                ))}
                {coreSevenths.length === 0 ? <div className="empty">No core sevenths found for this EDO.</div> : null}
              </div>
            </details>

            <details className="acc" open={expandAll}>
              <summary className="accSum">Tuning-specific</summary>
              <div className="accBody">
                {tuningSpecific.map((it) => (
                  <TuningChordDetails
                    key={chordKey(edo, it.bestName, it.pcsRoot)}
                    chordKey={chordKey(edo, it.bestName, it.pcsRoot)}
                    summary={it.bestName}
                    summaryTooltip={otherNamesTooltip(it.bestName, it.allNames)}
                    headerRight={headerButton(it.bestName, it.pcsRoot)}
                  >
                    <ChordBlock
                      edo={edo}
                      dq={dq}
                      dr={dr}
                      title={it.bestName}
                      pcsRoot={it.pcsRoot}
                      lib={it.pcsRoot.length === 3 ? libs.lib3 : libs.lib4}
                      expandAll={expandAll}
                    />
                  </TuningChordDetails>
                ))}
                {tuningSpecific.length === 0 ? <div className="empty">No tuning-specific chords found.</div> : null}
              </div>
            </details>
          </div>

          <div className="printRoot" aria-hidden="true">
            {(() => {
              const all = [
                ...coreTriads.map((c) => ({ title: c.item.bestName, pcsRoot: c.pcsRoot, lib: libs.lib3 })),
                ...coreSevenths.map((c) => ({ title: c.item.bestName, pcsRoot: c.pcsRoot, lib: libs.lib4 })),
                ...tuningSpecific.map((it) => ({ title: it.bestName, pcsRoot: it.pcsRoot, lib: it.pcsRoot.length === 3 ? libs.lib3 : libs.lib4 })),
              ]
              const want = new Set(printChordKeys)
              const selected = want.size
                ? all.filter((c) => want.has(chordKey(edo, c.title, c.pcsRoot)))
                : []

              return selected.map((c) => (
                <PrintChordPage
                  key={chordKey(edo, c.title, c.pcsRoot)}
                  edo={edo}
                  title={c.title}
                  pcsRoot={c.pcsRoot}
                  lib={c.lib}
                />
              ))
            })()}
          </div>
          </>
        ) : null}
      </main>
    </div>
  )
}

function fmtStep(n: number, edo: number) {
  const x = mod(n, edo)
  // Prefer a signed representative when that is simpler.
  if (x === 0) return '0'
  const neg = x - edo
  const best = Math.abs(neg) < Math.abs(x) ? neg : x
  if (best > 0) return `+${best}`
  return String(best)
}

function formatAxisSteps(edo: number, dq: number, dr: number, axis3Raw: number) {
  const axis3 = axis3Raw

  // Include both directions for each axis.
  // The labels are purely mnemonic; the guide remains transposable.
  return [
    { dir: 'L', text: `← ${fmtStep(-dq, edo)}` },
    { dir: 'LU', text: `↖ ${fmtStep(-dr, edo)}` },
    { dir: 'LD', text: `↙ ${fmtStep(axis3, edo)}` },
    { dir: 'RD', text: `↘ ${fmtStep(dr, edo)}` },
    { dir: 'RU', text: `↗ ${fmtStep(-axis3, edo)}` },
    { dir: 'R', text: `→ ${fmtStep(dq, edo)}` },
  ] as const
}

function otherNamesTooltip(mainTitle: string, allNames: string[]) {
  const main = String(mainTitle || '').trim().toLowerCase()
  const out: string[] = []
  const seen = new Set<string>()
  for (const n of allNames || []) {
    const s = String(n || '').trim()
    if (!s) continue
    if (s.toLowerCase() === main) continue
    const k = s.toLowerCase()
    if (seen.has(k)) continue
    seen.add(k)
    out.push(s)
  }
  if (out.length === 0) return ''
  return out.join('\n')
}

async function copyTextToClipboard(text: string): Promise<boolean> {
  // navigator.clipboard requires a secure context (HTTPS), except some browsers treat localhost as secure.
  // This guide is often used over LAN on plain HTTP, so keep a robust fallback.
  try {
    if (window.isSecureContext && navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
      await navigator.clipboard.writeText(text)
      return true
    }
  } catch {
    // fall through
  }

  try {
    const ta = document.createElement('textarea')
    ta.value = text
    ta.setAttribute('readonly', 'true')
    ta.style.position = 'fixed'
    ta.style.left = '-9999px'
    ta.style.top = '0'
    document.body.appendChild(ta)
    ta.focus()
    ta.select()
    const ok = document.execCommand('copy')
    document.body.removeChild(ta)
    return Boolean(ok)
  } catch {
    return false
  }
}

function ChordBlock({
  edo,
  dq,
  dr,
  title,
  pcsRoot,
  lib,
  expandAll,
}: {
  edo: number
  dq: number
  dr: number
  title: string
  pcsRoot: number[]
  lib: Map<string, Shape[]>
  expandAll: boolean
}) {
  const invs = useMemo(() => inversions(edo, pcsRoot), [edo, pcsRoot])

  return (
    <div className="chordBlock">
      <div className="invWrap">
        {invs.map((inv, i) => {
          const pcsKey = inv.pcsInv.join(',')
          const absSigKey = inv.pcsInv.join(',')
          const shapes: Shape[] = lib.get(`${pcsKey}::${absSigKey}`) || []
          const label =
            i === 0
              ? 'Root'
              : i === 1
                ? '1st inversion'
                : i === 2
                  ? '2nd inversion'
                  : i === 3
                    ? '3rd inversion'
                    : `${i}th inversion`

          return (
            <details key={pcsKey} className="invAcc" open={expandAll}>
              <summary className="invSum">{label}</summary>
              <div className="invBody">
                <InversionPatterns
                  edo={edo}
                  dq={dq}
                  dr={dr}
                  title={title}
                  pcsRoot={pcsRoot}
                  inversionIndex={i}
                  inv={inv}
                  shapes={shapes}
                  expandAll={expandAll}
                />
              </div>
            </details>
          )
        })}
      </div>
    </div>
  )
}
