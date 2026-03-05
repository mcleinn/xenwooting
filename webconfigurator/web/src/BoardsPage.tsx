import './BoardsPage.css'
import { useMemo } from 'react'
import { HEX_NEIGHBOR_DELTAS, LTN_GRIDS, type BoardGrid, validateAllGrids, WTN_GRIDS, xyKey } from './hexgrid/boardGrids'

function boundsFor(grid: BoardGrid) {
  let minX = Infinity
  let maxX = -Infinity
  let minY = Infinity
  let maxY = -Infinity
  for (const k of grid.keys) {
    minX = Math.min(minX, k.x)
    maxX = Math.max(maxX, k.x)
    minY = Math.min(minY, k.y)
    maxY = Math.max(maxY, k.y)
  }
  return { minX, maxX, minY, maxY }
}

type GridLayer = { grid: BoardGrid; label: string; color: string }

function boundsForLayers(layers: GridLayer[]) {
  let minX = Infinity
  let maxX = -Infinity
  let minY = Infinity
  let maxY = -Infinity
  for (const l of layers) {
    const b = boundsFor(l.grid)
    minX = Math.min(minX, b.minX)
    maxX = Math.max(maxX, b.maxX)
    minY = Math.min(minY, b.minY)
    maxY = Math.max(maxY, b.maxY)
  }
  return { minX, maxX, minY, maxY }
}

function CombinedGridSvg({ layers }: { layers: GridLayer[] }) {
  const { minX, maxX, minY, maxY } = useMemo(() => boundsForLayers(layers), [layers])

  const hx = 22
  const hy = 18
  const pad = 18

  const w = Math.max(1, (maxY - minY) * hx + pad * 2 + 30)
  const h = Math.max(1, (maxX - minX) * hy + pad * 2 + 30)

  const union = useMemo(() => {
    const byXY = new Map<
      string,
      { cx: number; cy: number; x: number; y: number; key: number; layerLabel: string; color: string }
    >()
    const pts: Array<{ cx: number; cy: number; x: number; y: number; key: number; layerLabel: string; color: string }> = []

    for (const l of layers) {
      for (const k of l.grid.keys) {
        const cx = pad + (k.y - minY) * hx
        const cy = pad + (k.x - minX) * hy
        const xy = xyKey(k.x, k.y)
        const p = { cx, cy, x: k.x, y: k.y, key: k.key, layerLabel: l.label, color: l.color }
        byXY.set(xy, p)
        pts.push(p)
      }
    }

    const edges: Array<{ x1: number; y1: number; x2: number; y2: number; stroke: string; opacity: number }> = []
    for (const p of pts) {
      for (const [dx, dy] of HEX_NEIGHBOR_DELTAS) {
        // Only draw half the undirected edges.
        if (dx < 0) continue
        if (dx === 0 && dy < 0) continue
        const nb = byXY.get(xyKey(p.x + dx, p.y + dy))
        if (!nb) continue
        const same = nb.layerLabel === p.layerLabel
        edges.push({
          x1: p.cx,
          y1: p.cy,
          x2: nb.cx,
          y2: nb.cy,
          stroke: same ? p.color : 'rgba(255,255,255,0.9)',
          opacity: same ? 0.22 : 0.18,
        })
      }
    }

    return { pts, edges }
  }, [layers, minX, minY])

  return (
    <svg className="gridSvg" viewBox={`0 0 ${w} ${h}`} role="img" aria-label="combined grid">
      {union.edges.map((e, i) => (
        <line
          key={`e:${i}`}
          className="gridEdge"
          x1={e.x1}
          y1={e.y1}
          x2={e.x2}
          y2={e.y2}
          style={{ stroke: e.stroke, opacity: e.opacity }}
        />
      ))}

      {union.pts.map((p) => (
        <g key={`${p.layerLabel}:${p.key}`} transform={`translate(${p.cx},${p.cy})`}>
          <title>{`${p.layerLabel} key ${p.key} @ (${p.x}, ${p.y})`}</title>
          <circle className="gridDot" r={9} style={{ fill: p.color, opacity: 0.14, stroke: p.color }} />
          <text className="gridLabel" textAnchor="middle" dominantBaseline="central">
            {p.key}
          </text>
        </g>
      ))}
    </svg>
  )
}

function ValidationRow({ label, ok, errors }: { label: string; ok: boolean; errors: string[] }) {
  return (
    <div className="valRow">
      <div className="valLabel">{label}</div>
      <div className={`boardStatus ${ok ? 'ok' : 'bad'}`}>{ok ? 'OK' : 'Issues'}</div>
      {!ok && <div className="valErr">{errors[0] || 'invalid'}</div>}
    </div>
  )
}

export default function BoardsPage() {
  const wtn = WTN_GRIDS
  const ltn = LTN_GRIDS
  const all = useMemo(() => validateAllGrids(), [])

  const wtnLayers: GridLayer[] = [
    { grid: wtn.Board0, label: 'WTN Board0', color: '#6EE7B7' },
    { grid: wtn.Board1, label: 'WTN Board1', color: '#60A5FA' },
  ]

  const ltnLayers: GridLayer[] = [
    { grid: ltn.Board0, label: 'LTN Board0', color: '#FCA5A5' },
    { grid: ltn.Board1, label: 'LTN Board1', color: '#FDBA74' },
    { grid: ltn.Board2, label: 'LTN Board2', color: '#FDE68A' },
    { grid: ltn.Board3, label: 'LTN Board3', color: '#A7F3D0' },
    { grid: ltn.Board4, label: 'LTN Board4', color: '#93C5FD' },
  ]

  return (
    <div className="boardsPage">
      <header className="boardsTop">
        <div className="boardsTitle">Board Grids</div>
      </header>

      <main className="boardsMain">
        <details className="acc" open>
          <summary className="accSum">WTN combined</summary>
          <div className="accBody">
            <div className="combo">
              <div className="comboTop">
                <div className="legend">
                  {wtnLayers.map((l) => (
                    <div key={l.label} className="legendItem">
                      <span className="legendSwatch" style={{ background: l.color }} />
                      <span>{l.label}</span>
                    </div>
                  ))}
                </div>
              </div>
              {!all.sets.wtn.ok && (
                <div className="setError">{all.sets.wtn.errors[0] || 'WTN set invalid'}</div>
              )}
              <div className="valWrap">
                <ValidationRow label="Board0" ok={all.wtn.Board0.ok} errors={all.wtn.Board0.errors} />
                <ValidationRow label="Board1" ok={all.wtn.Board1.ok} errors={all.wtn.Board1.errors} />
              </div>
              <CombinedGridSvg layers={wtnLayers} />
            </div>
          </div>
        </details>

        <details className="acc" open>
          <summary className="accSum">LTN combined</summary>
          <div className="accBody">
            <div className="combo">
              <div className="comboTop">
                <div className="legend">
                  {ltnLayers.map((l) => (
                    <div key={l.label} className="legendItem">
                      <span className="legendSwatch" style={{ background: l.color }} />
                      <span>{l.label}</span>
                    </div>
                  ))}
                </div>
              </div>
              {!all.sets.ltn.ok && (
                <div className="setError">{all.sets.ltn.errors[0] || 'LTN set invalid'}</div>
              )}
              <div className="valWrap">
                <ValidationRow label="Board0" ok={all.ltn.Board0.ok} errors={all.ltn.Board0.errors} />
                <ValidationRow label="Board1" ok={all.ltn.Board1.ok} errors={all.ltn.Board1.errors} />
                <ValidationRow label="Board2" ok={all.ltn.Board2.ok} errors={all.ltn.Board2.errors} />
                <ValidationRow label="Board3" ok={all.ltn.Board3.ok} errors={all.ltn.Board3.errors} />
                <ValidationRow label="Board4" ok={all.ltn.Board4.ok} errors={all.ltn.Board4.errors} />
              </div>
              <CombinedGridSvg layers={ltnLayers} />
            </div>
          </div>
        </details>
      </main>
    </div>
  )
}
