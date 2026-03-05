import './App.css'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  fetchGeometry,
  fetchLayout,
  fetchLayouts,
  fetchNoteNames,
  highlightKey,
  addLayout,
  updateLayoutSettings,
  deleteLayout,
  previewDisable,
  previewEnable,
  previewUpdate,
  saveLayout,
} from './api'
import { KeyboardView } from './KeyboardView'
import type { Boards, Geometry, LayoutInfo } from './types'
import { parseLtnText, type LtnData } from './ltn/parse'
import { buildWtnCombinedLookup, invRotateHex, rotateHex, subHex, addHex, type HexCoord } from './hexgrid/project'
import { HEX_NEIGHBOR_DELTAS, LTN_GRIDS, WTN_GRIDS, xyKey } from './hexgrid/boardGrids'

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

  const fileInputRef = useRef<HTMLInputElement | null>(null)
  const fileActionRef = useRef<'import' | 'add' | null>(null)

  const [addOpen, setAddOpen] = useState(false)
  const [addEdo, setAddEdo] = useState('')
  const [addName, setAddName] = useState('')
  const [addNameTouched, setAddNameTouched] = useState(false)
  const [addLtn, setAddLtn] = useState<LtnData | null>(null)
  const [pendingStart, setPendingStart] = useState<{ layoutId: string; ltn: LtnData; anchor: HexCoord } | null>(null)

  const [placement, setPlacement] = useState<
    | null
    | {
        ltn: LtnData
        rot: number
        tx: number
        ty: number
        anchor: HexCoord
      }
  >(null)

  const [settingsOpen, setSettingsOpen] = useState(false)
  const [settingsName, setSettingsName] = useState('')
  const [settingsEdo, setSettingsEdo] = useState('')

  const [enumOpen, setEnumOpen] = useState(false)
  const [enumInc, setEnumInc] = useState('1')
  const enumInputRef = useRef<HTMLInputElement | null>(null)

  const [hoveredKey, setHoveredKey] = useState<{ board: 'Board0' | 'Board1'; idx: number } | null>(null)

  // Display preference for enharmonic spellings on key labels.
  const [enharmLabelMode, setEnharmLabelMode] = useState<'both' | 'first' | 'second'>('both')

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
  const [noteNameCache, setNoteNameCache] = useState<
    Map<string, { short: string; unicode: string; alts: Array<{ short: string; unicode: string }> } | null>
  >(new Map())
  const noteNamesFetchTimer = useRef<number | null>(null)
  const noteNamesRetryTimer = useRef<number | null>(null)
  const [noteNamesRetryTick, setNoteNamesRetryTick] = useState(0)

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

    // Layout change also exits placement mode.
    setPlacement(null)

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
      if (placement) return
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
      palette.sort()
      if (palette.length < 2) return

      const { board, idx } = hoveredKey
      const cur = boards[board]?.[idx]
      if (!cur) return
      const curCol = String(cur.col || '').trim().toUpperCase()
      const at = palette.indexOf(curCol)
      const backwards = e.shiftKey
      const nextCol = backwards
        ? palette[(at >= 0 ? at - 1 + palette.length : palette.length - 1) % palette.length]
        : palette[(at >= 0 ? at + 1 : 0) % palette.length]
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
  }, [boards, hoveredKey, geometry, pushPreview, placement])

  const wtnLookup = useMemo(() => buildWtnCombinedLookup(), [])

  const defaultAnchor = useMemo((): HexCoord => {
    const c = WTN_GRIDS.Board0.byKey.get(0)
    return c ? { x: c.x, y: c.y } : { x: 0, y: 0 }
  }, [])

  const visMap = useMemo(() => {
    if (!geometry) return null
    const b0 = buildVisibleIndexMap(geometry, true)
    const b1 = buildVisibleIndexMap(geometry, false)
    return { Board0: b0, Board1: b1 }
  }, [geometry])

  const hoveredWorld = useMemo(() => {
    if (!hoveredKey || !visMap) return null
    const v = visMap[hoveredKey.board].visByInternal.get(hoveredKey.idx)
    if (v === undefined) return null
    const c = WTN_GRIDS[hoveredKey.board].byKey.get(v)
    return c ? ({ x: c.x, y: c.y } as HexCoord) : null
  }, [hoveredKey, visMap])

  const overlayByBoard = useMemo(() => {
    if (!placement || !boards || !visMap) return null
    const out = {
      Board0: new Map<number, { note: number; chan: number; col: string }>(),
      Board1: new Map<number, { note: number; chan: number; col: string }>(),
    }
    for (const [bStr, arr] of Object.entries(placement.ltn.boards)) {
      const bNum = Number.parseInt(bStr, 10)
      if (!Number.isFinite(bNum)) continue
      for (let k = 0; k < arr.length; k++) {
        const cell = arr[k]
        if (!cell) continue
        const src = ltnCoord(bNum, k)
        if (!src) continue
        const world = addHex(rotateHex(src, placement.rot), { x: placement.tx, y: placement.ty })
        const hit = wtnLookup.get(xyKey(world.x, world.y))
        if (!hit) continue
        const internalIdx = visMap[hit.board].internalByVis[hit.visKey]
        if (internalIdx === undefined) continue
        out[hit.board].set(internalIdx, { note: cell.note, chan: cell.chan, col: cell.col })
      }
    }
    return out
  }, [placement, boards, visMap, wtnLookup])

  const isoBadgeByBoard = useMemo(() => {
    if (!boards || !visMap) return null

    const bds = boards
    const vmap = visMap
    const overlay = overlayByBoard
    const edo = Math.max(1, edoDivisions)
    // Use the 3 positive-direction axes for undirected edge checks.
    const axisDirs = HEX_NEIGHBOR_DELTAS.filter(([dx, dy]) => dx > 0 || (dx === 0 && dy > 0))

    function axisInfo(dx: number, dy: number): { axis: string; dir: string; compass: string; fallback: string } {
      // Visual directions (in the combined grid):
      // - +y moves right
      // - +x moves down
      // Our 3 checked axes are pairs of opposite directions.
      const k = `${dx},${dy}`
      if (k === '0,2') return { axis: '←/→', dir: '→', compass: 'W/E', fallback: 'E' }
      if (k === '1,1') return { axis: '↖/↘', dir: '↘', compass: 'NW/SE', fallback: 'SE' }
      if (k === '1,-1') return { axis: '↗/↙', dir: '↙', compass: 'NE/SW', fallback: 'SW' }
      return { axis: `${dx},${dy}`, dir: `${dx},${dy}`, compass: '', fallback: `${dx},${dy}` }
    }

    function compute(board: 'Board0' | 'Board1') {
      const internalIdxs = vmap[board].internalByVis
      const pts: Array<{ x: number; y: number; pitch: number }> = []
      const pitchByXY = new Map<string, number>()
      const metaByXY = new Map<string, { idx: number; chan: number; note: number; pitch: number }>()
      let missing = 0

      for (const internalIdx of internalIdxs) {
        const visKey = vmap[board].visByInternal.get(internalIdx)
        if (visKey === undefined) {
          missing++
          continue
        }
        const coord = WTN_GRIDS[board].byKey.get(visKey)
        if (!coord) {
          missing++
          continue
        }

        const overlayCell = placement ? overlay?.[board].get(internalIdx) : undefined
        const baseCell = bds[board][internalIdx]
        const cell = overlayCell || baseCell
        if (!cell) {
          missing++
          continue
        }
        if (!overlayCell && baseCell?.set === false) {
          missing++
          continue
        }

        const pitch = (Math.max(0, Math.min(15, cell.chan - 1)) * edo + cell.note + pitchOffset) | 0
        const x = coord.x
        const y = coord.y
        pts.push({ x, y, pitch })
        pitchByXY.set(xyKey(x, y), pitch)
        metaByXY.set(xyKey(x, y), { idx: internalIdx, chan: cell.chan, note: cell.note, pitch })
      }

      const dirInfo = axisDirs.map(([dx, dy]) => {
        const info = axisInfo(dx, dy)
        const counts = new Map<number, number>()
        const edgesList: Array<{ from: { idx: number; chan: number; note: number; pitch: number }; to: { idx: number; chan: number; note: number; pitch: number }; dp: number }> = []
        let edges = 0
        for (const p of pts) {
          const from = metaByXY.get(xyKey(p.x, p.y))
          if (!from) continue
          const to = metaByXY.get(xyKey(p.x + dx, p.y + dy))
          if (!to) continue
          edges++
          const dp = (to.pitch - from.pitch) | 0
          counts.set(dp, (counts.get(dp) || 0) + 1)
          edgesList.push({ from, to, dp })
        }

        let expected: number | null = null
        let expectedCount = -1
        for (const [dp, c] of counts.entries()) {
          if (c > expectedCount) {
            expected = dp
            expectedCount = c
          }
        }
        const mismatches = expected !== null ? edgesList.filter((e) => e.dp !== expected) : []

        return { dx, dy, edges, counts, expected, mismatches, info }
      })

      const anyInconsistent = dirInfo.some((d) => d.counts.size > 1)
      const allPresentAndConstant = dirInfo.every((d) => d.edges > 0 && d.counts.size === 1)

      let variant: 'ok' | 'bad' | 'unknown' = 'unknown'
      let text = 'Iso?'
      if (allPresentAndConstant) {
        variant = 'ok'
        text = 'Iso'
      } else if (anyInconsistent) {
        variant = 'bad'
        text = 'Non-iso'
      }

      const fmtStep = (n: number) => {
        if (n > 0) return `+${n}`
        return String(n)
      }

      const lines: string[] = []
      lines.push(`Isomorphic layout check (${board})`)
      lines.push(`An isomorphic layout means: moving in the same grid direction always changes pitch by the same amount.`)
      lines.push(`{...} shows the different pitch steps observed in that direction; one value means consistent.`)
      lines.push(`Visible keys with pitch: ${pts.length}/${internalIdxs.length} (missing ${missing}).`)
      lines.push(`Using effective mapping${placement ? ' (includes placement overlay)' : ''}.`)

      for (const d of dirInfo) {
        const label = `${d.info.axis}${d.info.compass ? ` (${d.info.compass})` : ''}`
        if (d.edges === 0) {
          lines.push(`${label}: no neighbor links found`)
          continue
        }
        const vals = Array.from(d.counts.keys()).sort((a, b) => a - b)
        const shown = vals.slice(0, 6).map(fmtStep)
        const more = vals.length > 6 ? ', …' : ''
        lines.push(`${label}: ${d.edges} links; steps {${shown.join(', ')}${more}}`)
      }

      if (variant === 'bad') {
        lines.push('')
        lines.push('Examples that break isomorphism:')
        let shown = 0
        for (const d of dirInfo) {
          if (shown >= 12) break
          if (!d.mismatches.length || d.expected === null) continue
          for (const m of d.mismatches.slice(0, 4)) {
            if (shown >= 12) break
            lines.push(
              `${d.info.dir} (${d.info.fallback}): expected ${fmtStep(d.expected)} but got ${fmtStep(m.dp)} at idx ${m.from.idx} ch${m.from.chan}:${m.from.note} -> idx ${m.to.idx} ch${m.to.chan}:${m.to.note}`,
            )
            shown++
          }
        }
      } else if (variant === 'unknown') {
        const missingDirs = dirInfo.filter((d) => d.edges === 0).map((d) => `${d.info.axis}${d.info.compass ? ` (${d.info.compass})` : ''}`)
        if (missingDirs.length) {
          lines.push('')
          lines.push(`Cannot confirm isomorphism: no neighbor links for ${missingDirs.join(', ')}.`)
        }
      }

      return { text, variant, title: lines.join('\n') }
    }

    return {
      Board0: compute('Board0'),
      Board1: compute('Board1'),
    }
  }, [boards, visMap, overlayByBoard, placement, edoDivisions, pitchOffset])

  // Hotkey: 'e' toggles enharmonic spelling display globally (visual only).
  // Cycles: both -> first -> second -> both
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase()
      if (tag === 'input' || tag === 'textarea' || tag === 'select') return
      if (e.key.toLowerCase() !== 'e') return
      if (displayMode === 'number') return

      setEnharmLabelMode((m) => (m === 'both' ? 'first' : m === 'first' ? 'second' : 'both'))
      e.preventDefault()
    }

    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [displayMode])

  const startPlacement = useCallback(
    (ltn: LtnData, anchor: HexCoord) => {
      // Find a reasonable source pivot: first present cell in file.
      let srcPivot: HexCoord | null = null
      for (const bNum of [0, 1, 2, 3, 4]) {
        const arr = ltn.boards[bNum]
        if (!arr) continue
        for (let i = 0; i < arr.length; i++) {
          if (!arr[i]) continue
          const c = ltnCoord(bNum, i)
          if (c) {
            srcPivot = c
            break
          }
        }
        if (srcPivot) break
      }

      if (!srcPivot) {
        setStatus('Import failed: .ltn contains no complete key entries')
        return
      }

      const t0 = subHex(anchor, rotateHex(srcPivot, 0))
      setPlacement({ ltn, rot: 0, tx: t0.x, ty: t0.y, anchor })
      setStatus('')
    },
    [setPlacement],
  )

  const applyOverlay = useCallback(() => {
    if (!boards) return
    if (!overlayByBoard) return

    const next: Boards = {
      Board0: boards.Board0.map((c) => ({ ...c })),
      Board1: boards.Board1.map((c) => ({ ...c })),
    }

    for (const b of ['Board0', 'Board1'] as const) {
      for (const [idx, c] of overlayByBoard[b].entries()) {
        const cell = next[b][idx]
        if (!cell) continue
        cell.note = c.note
        cell.chan = c.chan
        cell.col = c.col
        cell.set = true
      }
    }

    setBoards(next)
    pushPreview(next)
    setPlacement(null)
    setStatus('Placement applied (not saved).')
    setTimeout(() => setStatus(''), 800)
  }, [boards, overlayByBoard, pushPreview])

  useEffect(() => {
    if (!placement) return
    const onKeyDown = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName?.toLowerCase()
      if (tag === 'input' || tag === 'textarea' || tag === 'select') return

      if (e.key === 'Escape') {
        e.preventDefault()
        setPlacement(null)
        setStatus('Placement aborted.')
        setTimeout(() => setStatus(''), 800)
        return
      }
      if (e.key === 'Enter') {
        e.preventDefault()
        applyOverlay()
        return
      }
      if (e.key === 'ArrowLeft') {
        e.preventDefault()
        setPlacement((p) => (p ? { ...p, ty: p.ty - 2 } : p))
        return
      }
      if (e.key === 'ArrowRight') {
        e.preventDefault()
        setPlacement((p) => (p ? { ...p, ty: p.ty + 2 } : p))
        return
      }
      if (e.key === 'ArrowUp') {
        e.preventDefault()
        setPlacement((p) => (p ? { ...p, tx: p.tx - 1, ty: p.ty - 1 } : p))
        return
      }
      if (e.key === 'ArrowDown') {
        e.preventDefault()
        setPlacement((p) => (p ? { ...p, tx: p.tx + 1, ty: p.ty + 1 } : p))
        return
      }
      if (e.key.toLowerCase() === 'r') {
        e.preventDefault()
        const pivot = hoveredWorld || placement.anchor
        setPlacement((p) => {
          if (!p) return p
          const w = pivot
          const srcPivot = invRotateHex(subHex(w, { x: p.tx, y: p.ty }), p.rot)
          const rot2 = (p.rot + 1) % 6
          const w2 = rotateHex(srcPivot, rot2)
          const t2 = subHex(w, w2)
          return { ...p, rot: rot2, tx: t2.x, ty: t2.y }
        })
      }
    }
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [placement, hoveredWorld, applyOverlay])

  // Prefetch note names for all keys (both boards) and cache them.
  useEffect(() => {
    if (!boards) return
    const edo = Math.max(1, edoDivisions)

    const missing: number[] = []
    const seenPitch = new Set<number>()

    // Base layout cells.
    for (const b of ['Board0', 'Board1'] as const) {
      for (const c of boards[b]) {
        // Note names are only meaningful for set cells.
        if (c.set === false) continue
        const pitch = (Math.max(0, Math.min(15, c.chan - 1)) * edo + c.note + pitchOffset) | 0
        if (seenPitch.has(pitch)) continue
        seenPitch.add(pitch)

        const k = `${edo}:${pitch}`
        if (!noteNameCache.has(k)) missing.push(pitch)
      }
    }

    // Placement overlay cells.
    if (placement && overlayByBoard) {
      for (const b of ['Board0', 'Board1'] as const) {
        for (const c of overlayByBoard[b].values()) {
          const pitch = (Math.max(0, Math.min(15, c.chan - 1)) * edo + c.note + pitchOffset) | 0
          if (seenPitch.has(pitch)) continue
          seenPitch.add(pitch)
          const k = `${edo}:${pitch}`
          if (!noteNameCache.has(k)) missing.push(pitch)
        }
      }
    }

    if (missing.length === 0) return

    // Keep batches small so the service stays responsive (especially for higher EDOs).
    const batch = missing.slice(0, 64)

    // Debounce so editing doesn't spam the server.
    if (noteNamesFetchTimer.current !== null) {
      window.clearTimeout(noteNamesFetchTimer.current)
      noteNamesFetchTimer.current = null
    }
    noteNamesFetchTimer.current = window.setTimeout(() => {
      fetchNoteNames(edo, batch)
        .then((r) => {
          const results = r?.results || {}
          setNoteNameCache((prev) => {
            const next = new Map(prev)
            for (const p of batch) {
              const v = results[String(p)]
              const key = `${edo}:${p}`
              if (v && typeof v.unicode === 'string' && typeof v.short === 'string') {
                const alts = Array.isArray(v.alts)
                  ? v.alts
                      .filter((x) => x && typeof x === 'object' && typeof x.unicode === 'string' && typeof x.short === 'string')
                      .map((x) => ({ short: x.short, unicode: x.unicode }))
                  : []
                next.set(key, { short: v.short, unicode: v.unicode, alts })
              }
            }
            return next
          })
        })
        .catch(() => {
          // Avoid poisoning the cache on transient failures; retry shortly.
          if (noteNamesRetryTimer.current !== null) return
          noteNamesRetryTimer.current = window.setTimeout(() => {
            noteNamesRetryTimer.current = null
            setNoteNamesRetryTick((x) => (x + 1) | 0)
          }, 450)
        })
    }, 80)

    return () => {
      if (noteNamesFetchTimer.current !== null) {
        window.clearTimeout(noteNamesFetchTimer.current)
        noteNamesFetchTimer.current = null
      }
      if (noteNamesRetryTimer.current !== null) {
        window.clearTimeout(noteNamesRetryTimer.current)
        noteNamesRetryTimer.current = null
      }
    }
  }, [boards, edoDivisions, pitchOffset, noteNameCache, placement, overlayByBoard, noteNamesRetryTick])

  const noteNamesByBoard = useMemo(() => {
    if (!boards) return null
    const edo = Math.max(1, edoDivisions)
    const b0 = new Map<number, string>()
    const b1 = new Map<number, string>()

    const showForPitch = (pitch: number): string | null => {
      const pitchKey = `${edo}:${pitch}`
      const v = noteNameCache.get(pitchKey)
      if (!v || !v.unicode) return null

      const a = v.unicode
      const b = v.alts && v.alts.length ? v.alts[0]?.unicode : ''
      if (enharmLabelMode === 'first') return a
      if (enharmLabelMode === 'second') return b || a
      return b ? `${a}/${b}` : a
    }
    for (const [b, out] of [
      ['Board0', b0],
      ['Board1', b1],
    ] as const) {
      for (let i = 0; i < boards[b].length; i++) {
        const overlay = placement ? overlayByBoard?.[b].get(i) : undefined
        if (overlay) {
          const pitch = (Math.max(0, Math.min(15, overlay.chan - 1)) * edo + overlay.note + pitchOffset) | 0
          const uni = showForPitch(pitch)
          if (uni) out.set(i, uni)
          continue
        }

        const c = boards[b][i]
        if (c.set === false) continue
        const pitch = (Math.max(0, Math.min(15, c.chan - 1)) * edo + c.note + pitchOffset) | 0
        const uni = showForPitch(pitch)
        if (uni) out.set(i, uni)
      }
    }
    return { Board0: b0, Board1: b1 }
  }, [boards, edoDivisions, pitchOffset, noteNameCache, placement, overlayByBoard, enharmLabelMode])

  const noteTooltipByBoard = useMemo(() => {
    if (!boards) return null
    const edo = Math.max(1, edoDivisions)
    const mk = () => new Map<number, string>()
    const out = { Board0: mk(), Board1: mk() }
    for (const b of ['Board0', 'Board1'] as const) {
      for (let i = 0; i < boards[b].length; i++) {
        const overlay = placement ? overlayByBoard?.[b].get(i) : undefined
        const cell = overlay || boards[b][i]
        if (!cell) continue
        if (!overlay && (boards[b][i].set === false)) continue
        const pitch = (Math.max(0, Math.min(15, cell.chan - 1)) * edo + cell.note + pitchOffset) | 0
        const v = noteNameCache.get(`${edo}:${pitch}`)
        if (!v) continue
        const lines: string[] = []
        // Native browser tooltips (title=...) can't be styled, so avoid Bravura-only glyphs here.
        // If we have an enharmonic alternative, show the first two spellings as `A/B`.
        const a = v.short
        const b2 = v.alts && v.alts.length ? v.alts[0]?.short : ''
        lines.push(b2 ? `${a}/${b2}` : a)

        // If there are more alternatives beyond the first, list them on the next line.
        if (v.alts && v.alts.length > 1) {
          lines.push('Alt: ' + v.alts.slice(1).map((x) => x.short).join(' | '))
        }
        out[b].set(i, lines.join('\n'))
      }
    }
    return out
  }, [boards, edoDivisions, pitchOffset, noteNameCache, placement, overlayByBoard])

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
      if (cell.set === false) continue

      const ch0 = Math.max(0, Math.min(15, cell.chan - 1))
      const pitch = (ch0 * edo + cell.note + pitchOffset) | 0
      const hz = Math.round(C0_HZ * Math.pow(2, pitch / edo))

      const nn = noteNameCache.get(`${edo}:${pitch}`)
      const unicode = nn && nn.unicode ? nn.unicode : ''
      out.push({ id, pitch, hz, unicode })
    }

    return out
  }, [boards, selected, edoDivisions, pitchOffset, noteNameCache])

  useEffect(() => {
    if (!pendingStart) return
    if (layoutId !== pendingStart.layoutId) return
    if (!boards) return
    // eslint-disable-next-line react-hooks/set-state-in-effect
    startPlacement(pendingStart.ltn, pendingStart.anchor)
    setPendingStart(null)
  }, [pendingStart, layoutId, boards, startPlacement])

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
      cell.set = true
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
    const incRaw = Number.parseInt(enumInc.trim(), 10)
    if (!Number.isFinite(incRaw) || incRaw === 0) {
      setStatus('Enumerate: increment must be a non-zero integer.')
      setTimeout(() => setStatus(''), 1800)
      return
    }
    const inc = Math.max(-127, Math.min(127, incRaw | 0))

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
    const baseChan = clampInt(String(baseCell.chan | 0), 1, 16)
    const edo = Math.max(1, edoDivisions)
    const basePitch = ((Math.max(0, Math.min(15, baseChan - 1)) * edo + baseNote + pitchOffset) | 0) as number

    const next: Boards = {
      Board0: boards.Board0.map((c) => ({ ...c })),
      Board1: boards.Board1.map((c) => ({ ...c })),
    }

    let stoppedAt: number | null = null
    for (let k = 0; k < ordered.length; k++) {
      const { board, idx } = ordered[k]
      const cell = next[board][idx]
      if (!cell) continue
      cell.set = true

      const targetPitch = (basePitch + k * inc) | 0

      // Try to keep channel as low as possible, but when the note would exceed 127,
      // represent the same pitch using the next channel (note -= edo each step).
      let chan = baseChan
      let note = (targetPitch - pitchOffset - (chan - 1) * edo) | 0
      while (note > 127 && chan < 16) {
        chan++
        note -= edo
      }
      while (note < 0 && chan > 1) {
        chan--
        note += edo
      }

      if (note < 0 || note > 127) {
        stoppedAt = k
        break
      }

      cell.chan = chan
      cell.note = note
    }

    setBoards(next)
    pushPreview(next)
    closeEnumerate()

    if (stoppedAt !== null) {
      setStatus(`Enumerate stopped at item ${stoppedAt + 1}: cannot represent pitch within chan 1..16 and note 0..127.`)
      setTimeout(() => setStatus(''), 2400)
    }
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
    setPlacement(null)
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
    setPlacement(null)
    setStatus('Saving...')
    try {
      const r = await saveLayout(layoutId, boards)
      await exitPreviewMode()
      setStatus(r.xenwootingReloaded ? 'Saved + reloaded daemon.' : 'Saved (reload failed).')
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

  function openLayoutSettings() {
    if (!layoutId) return
    setSettingsName(layoutName)
    setSettingsEdo(String(edoDivisions))
    setSettingsOpen(true)
  }

  async function onSaveLayoutSettings() {
    if (!layoutId) return
    const name = settingsName.trim() || layoutId
    const edo = clampInt(settingsEdo, 1, 999)
    setPlacement(null)
    await exitPreviewMode()
    setStatus('Updating layout...')
    try {
      await updateLayoutSettings(layoutId, { name, edoDivisions: edo })
      const l = await fetchLayouts()
      setLayouts(l.layouts)
      const d = await fetchLayout(layoutId)
      setLayoutName(d.name)
      setEdoDivisions(d.edoDivisions)
      setPitchOffset(d.pitchOffset)
      setBoards(d.boards)
      setSettingsOpen(false)
      setStatus('')
    } catch (e) {
      setStatus(`Update failed: ${errMsg(e)}`)
    }
  }

  async function onDeleteLayout() {
    if (!layoutId) return
    if (layouts.length <= 1) return
    const ok = window.confirm(`Delete layout '${layoutName}'? This will also delete its .wtn file.`)
    if (!ok) return
    setPlacement(null)
    await exitPreviewMode()
    setStatus('Deleting layout...')
    try {
      const r = await deleteLayout(layoutId)
      const l = await fetchLayouts()
      setLayouts(l.layouts)
      setSettingsOpen(false)
      setStatus('')
      setLayoutId(r.nextId || (l.layouts[0]?.id || ''))
    } catch (e) {
      setStatus(`Delete failed: ${errMsg(e)}`)
    }
  }

  function onEditorEnter(e: React.KeyboardEvent<HTMLInputElement>) {
    if (e.key !== 'Enter') return
    if (placement) return
    if (enumOpen || addOpen || settingsOpen) return
    if (!boards || selected.size === 0) return
    e.preventDefault()
    applyEdits()
  }

  function onKeyHighlight(board: 'Board0' | 'Board1', idx: number, down: boolean) {
    if (!layoutId) return
    highlightKey(layoutId, board, idx, down).catch(() => {})
  }

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <div className="brandTitle">XenWTN configurator</div>
        </div>

        <div className="controls">
          <input
            ref={fileInputRef}
            type="file"
            accept=".ltn"
            style={{ display: 'none' }}
            onChange={(e) => {
              const f = e.target.files?.[0] || null
              if (!f) return
              const action = fileActionRef.current
              fileActionRef.current = null
              e.currentTarget.value = ''

              void f
                .text()
                .then((t) => {
                  const ltn = parseLtnText(t)
                  if (action === 'import') {
                    const anchor = hoveredWorld || defaultAnchor
                    startPlacement(ltn, { x: anchor.x, y: anchor.y })
                    return
                  }
                  if (action === 'add') {
                    const base = f.name.replace(/\.ltn$/i, '')
                    const m = base.match(/(\d+)/)
                    const edoFromName = m ? Number.parseInt(m[1] || '', 10) : NaN
                    const edoDefault = Number.isFinite(edoFromName) ? String(edoFromName) : String(edoDivisions)

                    setAddLtn(ltn)
                    setAddEdo(edoDefault)
                    setAddName(base)
                    // Keep filename-based name stable even if EDO changes.
                    setAddNameTouched(true)
                    setAddOpen(true)
                  }
                })
                .catch((err) => setStatus(`LTN parse failed: ${errMsg(err)}`))
            }}
          />

          <button
            className="btnSecondary"
            type="button"
            onClick={() => {
              window.open('/wtn/boards', '_blank', 'noopener,noreferrer')
            }}
            title="Open board configuration in a new window"
          >
            Board config
          </button>

          <button
            className="btnSecondary"
            type="button"
            onClick={() => {
              if (!fileInputRef.current) return
              fileActionRef.current = 'import'
              fileInputRef.current.click()
            }}
            disabled={!boards || !geometry || placement !== null}
            title="Import .ltn into current layout (placement mode)"
          >
            Import...
          </button>

          <button
            className="btnSecondary"
            type="button"
            onClick={() => {
              if (!fileInputRef.current) return
              fileActionRef.current = 'add'
              fileInputRef.current.click()
            }}
            disabled={!geometry || placement !== null}
            title="Add a new layout from .ltn (placement mode)"
          >
            Add layout...
          </button>

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
            onClick={openLayoutSettings}
            disabled={!layoutId || placement !== null}
          >
            Layout settings
          </button>

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
        {settingsOpen && (
          <div
            className="modalOverlay"
            onPointerDown={(e) => {
              if (e.target === e.currentTarget) setSettingsOpen(false)
            }}
          >
            <div
              className="modal"
              role="dialog"
              aria-modal="true"
              aria-label="Layout settings"
              onPointerDown={(e) => e.stopPropagation()}
            >
              <div className="modalTitle">Layout Settings</div>
              <div className="modalSub">Rename this layout or change its octave size.</div>

              <div className="grid">
                <label className="field">
                  <span className="fieldLabel">Layout name</span>
                  <input className="input" value={settingsName} onChange={(e) => setSettingsName(e.target.value)} />
                </label>
                <label className="field">
                  <span className="fieldLabel">Octave size (EDO divisions)</span>
                  <input
                    className="input"
                    inputMode="numeric"
                    value={settingsEdo}
                    onChange={(e) => setSettingsEdo(e.target.value)}
                  />
                </label>
              </div>

              <div className="row" style={{ justifyContent: 'space-between' }}>
                <button className="btnDanger" type="button" onClick={() => void onDeleteLayout()} disabled={layouts.length <= 1}>
                  Delete layout
                </button>
                <div className="row">
                  <button className="btnSecondary" type="button" onClick={() => setSettingsOpen(false)}>
                    Cancel
                  </button>
                  <button className="btn" type="button" onClick={() => void onSaveLayoutSettings()}>
                    Save
                  </button>
                </div>
              </div>
            </div>
          </div>
        )}

        {addOpen && (
          <div
            className="modalOverlay"
            onPointerDown={(e) => {
              if (e.target === e.currentTarget) setAddOpen(false)
            }}
          >
            <div
              className="modal"
              role="dialog"
              aria-modal="true"
              aria-label="Add layout"
              onPointerDown={(e) => e.stopPropagation()}
            >
              <div className="modalTitle">Add Layout</div>
              <div className="modalSub">Create a new layout from an .ltn file.</div>

              <div className="grid">
                <label className="field">
                  <span className="fieldLabel">Octave size (EDO divisions)</span>
                  <input
                    className="input"
                    inputMode="numeric"
                    value={addEdo}
                    onChange={(e) => {
                      const v = e.target.value
                      setAddEdo(v)
                      if (!addNameTouched) {
                        const n = clampInt(v, 1, 999)
                        setAddName(`${n}-EDO`)
                      }
                    }}
                  />
                </label>
                <label className="field">
                  <span className="fieldLabel">Layout name</span>
                  <input
                    className="input"
                    value={addName}
                    onChange={(e) => {
                      setAddNameTouched(true)
                      setAddName(e.target.value)
                    }}
                  />
                </label>
              </div>

              <div className="row">
                <button className="btnSecondary" type="button" onClick={() => setAddOpen(false)}>
                  Abort
                </button>
                <button
                  className="btn"
                  type="button"
                  onClick={() => {
                    const edo = clampInt(addEdo, 1, 999)
                    const name = addName.trim() || `${edo}-EDO`
                    const ltn = addLtn
                    if (!ltn) {
                      setAddOpen(false)
                      return
                    }
                    const anchor = hoveredWorld || defaultAnchor
                    setStatus('Adding layout...')
                    void addLayout({ name, edoDivisions: edo, pitchOffset: 0 })
                      .then((r) => fetchLayouts().then((l) => ({ r, l })))
                      .then(({ r, l }) => {
                        setLayouts(l.layouts)
                        setAddOpen(false)
                        setStatus('')
                        setPendingStart({ layoutId: r.id, ltn, anchor: { x: anchor.x, y: anchor.y } })
                        setLayoutId(r.id)
                      })
                      .catch((err) => setStatus(`Add layout failed: ${errMsg(err)}`))
                  }}
                >
                  Create
                </button>
              </div>
            </div>
          </div>
        )}

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
                <span className="fieldLabel">Increment (non-zero integer)</span>
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
                  onKeyDown={onEditorEnter}
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
                  onKeyDown={onEditorEnter}
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
                    onKeyDown={onEditorEnter}
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
            {placement && (
              <div className="status">
                Placement mode: arrows move, r rotates, Enter applies, Esc aborts.
              </div>
            )}
            <div className="hint">
              Click to select. Ctrl/Meta to multi-select. Shift to add range. Blank fields keep values. Press `c` to cycle the hovered key’s color through the currently visible colors. Press `e` to toggle enharmonic label display (first / second / both).
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
                noteTooltipByIdx={noteTooltipByBoard?.Board0}
                displayMode={displayMode}
                isoBadge={isoBadgeByBoard?.Board0}
                overlayByIdx={overlayByBoard?.Board0}
                placementActive={placement !== null}
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
                noteTooltipByIdx={noteTooltipByBoard?.Board1}
                displayMode={displayMode}
                isoBadge={isoBadgeByBoard?.Board1}
                overlayByIdx={overlayByBoard?.Board1}
                placementActive={placement !== null}
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

function buildVisibleIndexMap(
  geometry: Geometry,
  rotate180: boolean,
): { internalByVis: number[]; visByInternal: Map<number, number> } {
  const keys = geometry.keys
  const minColByRow = (() => {
    if (!rotate180) return [0, 0, 0, 0]
    const min = [255, 255, 255, 255]
    for (const k of keys) {
      const rr = 3 - k.row
      const cc = 13 - k.col
      min[rr] = Math.min(min[rr], cc)
    }
    for (let i = 0; i < 4; i++) if (min[i] === 255) min[i] = 0
    return min
  })()

  const tmp: Array<{ rr: number; cc: number; idx: number }> = []
  for (const k of keys) {
    const rr = rotate180 ? 3 - k.row : k.row
    const cc0 = rotate180 ? 13 - k.col : k.col
    const cc = cc0 - (minColByRow[rr] || 0)
    const idx = rr * 14 + cc
    tmp.push({ rr, cc, idx })
  }
  tmp.sort((a, b) => (a.rr - b.rr) * 100 + (a.cc - b.cc))
  const internalByVis = tmp.map((t) => t.idx)
  const visByInternal = new Map<number, number>()
  for (let i = 0; i < internalByVis.length; i++) visByInternal.set(internalByVis[i]!, i)
  return { internalByVis, visByInternal }
}

function ltnCoord(boardNum: number, keyIdx: number): HexCoord | null {
  const g =
    boardNum === 0
      ? LTN_GRIDS.Board0
      : boardNum === 1
        ? LTN_GRIDS.Board1
        : boardNum === 2
          ? LTN_GRIDS.Board2
          : boardNum === 3
            ? LTN_GRIDS.Board3
            : boardNum === 4
              ? LTN_GRIDS.Board4
              : null
  if (!g) return null
  const c = g.byKey.get(keyIdx)
  return c ? { x: c.x, y: c.y } : null
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
