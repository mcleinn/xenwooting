import { useEffect, useMemo, useRef, useState } from 'react'
import './LivePage.css'
import { fetchChordNames, fetchNoteNames } from './api'

type LiveState = {
  version: number
  seq: number
  ts_ms: number
  layout: { id: string; name: string; edo: number; pitch_offset: number }
  mode: { press_threshold: number; aftertouch: string; octave_shift: number }
  pressed: { Board0?: number[]; Board1?: number[] }
  layout_pitches: { Board0?: Array<number | null>; Board1?: Array<number | null> }
}

type NoteName = { short: string; unicode: string; alts?: Array<{ short: string; unicode: string }> }
type ChordRootResult = { rootPc: number; rel: number[]; pattern: string; names: string[] }

function apiUrl(path: string) {
  // Always resolve from /wtn/ so /wtn/live and /wtn/live/ behave the same.
  return new URL(path.replace(/^\//, ''), `${window.location.origin}/wtn/`).toString()
}

function mod(n: number, m: number) {
  const x = n % m
  return x < 0 ? x + m : x
}

function englishInterval12(semitones: number): string {
  // 0..11 only
  const s = mod(semitones, 12)
  switch (s) {
    case 0:
      return 'Unison'
    case 1:
      return 'Minor 2'
    case 2:
      return 'Major 2'
    case 3:
      return 'Minor 3'
    case 4:
      return 'Major 3'
    case 5:
      return 'Perfect 4'
    case 6:
      return 'Tritone'
    case 7:
      return 'Perfect 5'
    case 8:
      return 'Minor 6'
    case 9:
      return 'Major 6'
    case 10:
      return 'Minor 7'
    case 11:
      return 'Major 7'
    default:
      return ''
  }
}

function nameScore(name: string) {
  const s = String(name || '')
  const lower = s.toLowerCase()
  let score = 0
  if (lower.includes('inversion')) score += 1000
  if (lower.includes('2nd inversion')) score += 30
  if (lower.includes('1st inversion')) score += 20
  if (lower.includes('3rd inversion')) score += 40
  if (lower.includes('4th inversion')) score += 50

  // Prefer shorter, widely-readable names over very technical ones.
  if (lower.startsWith('nm ')) score += 350
  if (lower.includes('split fifth')) score += 180
  if (lower.includes('|')) score += 90
  if (lower.includes('quasi-')) score += 80
  if (lower.includes('ultra-gothic')) score += 120
  if (lower.includes('tredecimal')) score += 80
  if (lower.includes('trevicesimal')) score += 80
  if (lower.includes('bivalent')) score += 60
  if (lower.includes('subfocal')) score += 60
  if (lower.includes('isoharmonic')) score += 60
  if (lower.includes('neo-medieval')) score += 100

  // Prefer shorter, cleaner names.
  score += Math.min(200, s.length)
  // Penalize very "busy" names a bit.
  score += (s.match(/[()"']/g) || []).length * 8
  score += (s.match(/,/g) || []).length * 4
  return score
}

function bestName(names: string[]) {
  if (!Array.isArray(names) || names.length === 0) return ''
  const sorted = [...names].filter(Boolean).sort((a, b) => nameScore(a) - nameScore(b) || a.localeCompare(b))
  return sorted[0] || ''
}

function rootResultScore(r: ChordRootResult) {
  const n = Array.isArray(r.names) ? r.names : []
  const hasNames = n.length > 0
  const bn = bestName(n)
  const bnLower = bn.toLowerCase()
  const isInversion = bnLower.includes('inversion')
  const tones = Array.isArray(r.rel) ? r.rel.length : 0
  // Lower is better.
  return (
    (hasNames ? 0 : 10_000) +
    (isInversion ? 1_000 : 0) +
    tones * 10 +
    (bn ? nameScore(bn) : 0)
  )
}

function formatNoteUnicode(v: NoteName | null | undefined) {
  if (!v?.unicode) return ''
  const alt = v.alts && v.alts.length ? v.alts[0]?.unicode : ''
  return alt ? `${v.unicode}/${alt}` : v.unicode
}

function uniqSorted(nums: number[]) {
  const s = new Set<number>()
  for (const n of nums) s.add(n)
  return Array.from(s.values()).sort((a, b) => a - b)
}

function fitText(el: HTMLElement, container: HTMLElement, startPx: number, minPx: number) {
  let size = startPx
  el.style.fontSize = `${size}px`
  // Keep a little padding so it doesn't touch edges.
  const maxW = container.clientWidth * 0.96
  const maxH = container.clientHeight * 0.60
  while (size > minPx && (el.scrollWidth > maxW || el.scrollHeight > maxH)) {
    size -= 2
    el.style.fontSize = `${size}px`
  }
}

export default function LivePage() {
  const [live, setLive] = useState<LiveState | null>(null)
  const [view, setView] = useState<'notes' | 'pcs' | 'delta' | 'intervals'>('notes')
  const [noteCache, setNoteCache] = useState<Map<string, NoteName | null>>(new Map())
  const [chord, setChord] = useState<ChordRootResult[]>([])
  const lastLayoutKey = useRef<string>('')
  const fetchLock = useRef(false)
  const lastSeq = useRef<number>(-1)
  const noteCacheRef = useRef(noteCache)

  const [namesPopover, setNamesPopover] = useState<{ open: boolean; title: string; names: string[] }>({
    open: false,
    title: '',
    names: [],
  })
  const popoverTimer = useRef<number | null>(null)

  const mainWrapRef = useRef<HTMLDivElement | null>(null)
  const mainTextRef = useRef<HTMLDivElement | null>(null)

  // Subscribe to live state.
  useEffect(() => {
    let closed = false
    const es = new EventSource(apiUrl('api/live/stream'))
    es.addEventListener('state', (ev: MessageEvent) => {
      if (closed) return
      try {
        const obj: unknown = JSON.parse(String(ev.data || 'null'))
        if (!obj || typeof obj !== 'object') return
        const seqRaw = (obj as { seq?: unknown }).seq
        const seq = typeof seqRaw === 'number' ? seqRaw : Number.NaN
        if (Number.isFinite(seq) && seq === lastSeq.current) return
        if (Number.isFinite(seq)) lastSeq.current = seq
        setLive(obj as LiveState)
      } catch {
        // ignore
      }
    })
    es.onerror = () => {
      // EventSource will retry; keep silent.
    }
    return () => {
      closed = true
      es.close()
    }
  }, [])

  useEffect(() => {
    noteCacheRef.current = noteCache
  }, [noteCache])

  const edo = live?.layout?.edo || 12
  const pitchOffset = live?.layout?.pitch_offset || 0
  const layoutName = live?.layout?.name || live?.layout?.id || 'Live'

  const pressed = useMemo(() => {
    const a = Array.isArray(live?.pressed?.Board0) ? live!.pressed.Board0! : []
    const b = Array.isArray(live?.pressed?.Board1) ? live!.pressed.Board1! : []
    return uniqSorted([...a, ...b])
  }, [live])

  const pitchClasses = useMemo(() => {
    return uniqSorted(pressed.map((p) => mod(p, edo)))
  }, [pressed, edo])

  const pitchClassesKey = useMemo(() => pitchClasses.join(','), [pitchClasses])
  const pressedKey = useMemo(() => pressed.join(','), [pressed])

  const layoutId = live?.layout?.id || ''
  const octaveShift = live?.mode?.octave_shift ?? 0
  const layoutPitchesAll = useMemo(() => {
    const lp0 = Array.isArray(live?.layout_pitches?.Board0) ? live!.layout_pitches.Board0! : []
    const lp1 = Array.isArray(live?.layout_pitches?.Board1) ? live!.layout_pitches.Board1! : []
    const out: number[] = []
    for (const x of [...lp0, ...lp1]) {
      if (typeof x === 'number' && Number.isFinite(x)) out.push(x)
    }
    return out
  }, [live])

  // Prefetch note names for the current layout mapping (both boards), plus any currently pressed pitches.
  useEffect(() => {
    if (!layoutId) return
    const key = `${layoutId}:${edo}:${pitchOffset}:${octaveShift}`
    const wantFull = key && key !== lastLayoutKey.current
    const pitches: number[] = []

    if (wantFull) {
      for (const x of layoutPitchesAll) pitches.push(x)
      lastLayoutKey.current = key
    }

    for (const p of pressed) pitches.push(p)
    const uniq = uniqSorted(pitches)
    if (uniq.length === 0) return

    // Small lock so we don't run overlapping batches.
    if (fetchLock.current) return
    fetchLock.current = true

    const batches: number[][] = []
    for (let i = 0; i < uniq.length; i += 64) batches.push(uniq.slice(i, i + 64))

    ;(async () => {
      try {
        for (const batch of batches) {
          // Only fetch missing keys.
          const missing = batch.filter((p) => !noteCacheRef.current.has(`${edo}:${p}`))
          if (missing.length === 0) continue
          const r = await fetchNoteNames(edo, missing)
          const results = r?.results || {}
          setNoteCache((prev) => {
            const next = new Map(prev)
            for (const p of missing) {
              const v = results[String(p)]
              if (v && typeof v.unicode === 'string' && typeof v.short === 'string') next.set(`${edo}:${p}`, v)
              else next.set(`${edo}:${p}`, null)
            }
            return next
          })
        }
      } finally {
        fetchLock.current = false
      }
    })().catch(() => {
      fetchLock.current = false
    })
  }, [layoutId, edo, pitchOffset, octaveShift, layoutPitchesAll, pressedKey, pressed])

  // Fetch chord names (Scala db) for the combined pitch-class set.
  useEffect(() => {
    if (pitchClasses.length === 0) {
      if (chord.length) setChord([])
      return
    }
    const t = window.setTimeout(() => {
      fetchChordNames(edo, pitchClasses)
        .then((r) => setChord(Array.isArray(r?.results) ? r.results : []))
        .catch(() => {})
    }, 60)
    return () => window.clearTimeout(t)
  }, [edo, pitchClassesKey, pitchClasses.length, chord.length, pitchClasses])

  const mainText = useMemo(() => {
    if (pressed.length === 0) return ''
    if (view === 'pcs') {
      return pressed
        .map((p) => mod(p, edo))
        .sort((a, b) => a - b)
        .join(' ')
    }
    if (view === 'delta') {
      const rootPitch = pressed[0]
      const rootPc = mod(rootPitch, edo)
      const rootName = formatNoteUnicode(noteCache.get(`${edo}:${rootPitch}`) || null) || `pc${rootPc}`
      const deltas = uniqSorted(pressed.map((p) => mod(p - rootPitch, edo))).filter((d) => d !== 0)
      const parts = [`${rootName} (${rootPc})`, ...deltas.map((d) => `+${d}`)]
      return parts.join(' ')
    }
    // notes / intervals
    return pressed
      .map((p) => formatNoteUnicode(noteCache.get(`${edo}:${p}`) || null) || String(p))
      .join(' ')
  }, [pressed, view, edo, noteCache])

  const intervalLines = useMemo(() => {
    if (pitchClasses.length < 2) return []

    const wantAllRoots = view === 'intervals'

    // Derive a root pitch name per rootPc.
    const rootPitchByPc = new Map<number, number>()
    for (const p of pressed) {
      const pc = mod(p, edo)
      const cur = rootPitchByPc.get(pc)
      if (cur === undefined || p < cur) rootPitchByPc.set(pc, p)
    }

    const rootsRaw: ChordRootResult[] = chord.length
      ? chord
      : pitchClasses.map((rootPc) => ({ rootPc, rel: [], pattern: '', names: [] }))

    const rootsSorted = [...rootsRaw].sort((a, b) => {
      const sa = rootResultScore(a)
      const sb = rootResultScore(b)
      if (sa !== sb) return sa - sb
      return a.rootPc - b.rootPc
    })

    const withNames = rootsSorted.filter((r) => Array.isArray(r.names) && r.names.length > 0)
    const roots = wantAllRoots ? (withNames.length ? withNames : rootsSorted.slice(0, 1)) : rootsSorted.slice(0, 1)

    const out: Array<{
      rootPc: number
      rootName: string
      deltaText: string
      bestName: string
      allNames: string[]
    }> = []

    for (const r of roots) {
      const rootPitch = rootPitchByPc.get(r.rootPc)
      const rootName =
        rootPitch !== undefined ? formatNoteUnicode(noteCache.get(`${edo}:${rootPitch}`) || null) : `pc${r.rootPc}`

      const rel = Array.isArray(r.rel) && r.rel.length
        ? r.rel
        : pitchClasses.map((pc) => mod(pc - r.rootPc, edo)).sort((a, b) => a - b)
      const deltas = rel.filter((d) => d !== 0)
      const deltaText = deltas
        .map((d) => {
          if (edo === 12) return `+${d} (${englishInterval12(d)})`
          return `+${d}`
        })
        .join(' ')

      const allNames = Array.isArray(r.names)
        ? [...r.names].filter(Boolean).sort((a, b) => nameScore(a) - nameScore(b) || a.localeCompare(b))
        : []
      const best = bestName(allNames)

      out.push({ rootPc: r.rootPc, rootName, deltaText, bestName: best, allNames })
    }

    return out
  }, [pitchClasses, chord, pressed, edo, noteCache, view])

  const closePopover = () => {
    if (popoverTimer.current !== null) {
      window.clearTimeout(popoverTimer.current)
      popoverTimer.current = null
    }
    if (namesPopover.open) setNamesPopover({ open: false, title: '', names: [] })
  }

  const openPopover = (title: string, names: string[]) => {
    if (popoverTimer.current !== null) {
      window.clearTimeout(popoverTimer.current)
      popoverTimer.current = null
    }
    setNamesPopover({ open: true, title, names })
    popoverTimer.current = window.setTimeout(() => {
      popoverTimer.current = null
      setNamesPopover({ open: false, title: '', names: [] })
    }, 4000)
  }

  useEffect(() => {
    const wrap = mainWrapRef.current
    const el = mainTextRef.current
    if (!wrap || !el) return
    // Reset quickly then fit.
    el.style.fontSize = '180px'
    fitText(el, wrap, 180, 22)
  }, [mainText])

  useEffect(() => {
    const onResize = () => {
      const wrap = mainWrapRef.current
      const el = mainTextRef.current
      if (!wrap || !el) return
      fitText(el, wrap, 180, 22)
    }
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [])

  const onToggleView = () => {
    setView((v) => (v === 'notes' ? 'pcs' : v === 'pcs' ? 'delta' : v === 'delta' ? 'intervals' : 'notes'))
  }

  return (
    <div
      className="liveRoot"
      onPointerUp={(e) => {
        // Avoid triggering twice on touch devices.
        e.preventDefault()
        const t = e.target as HTMLElement | null
        if (t?.closest('.liveIntervals')) return
        if (t?.closest('.livePopover')) return
        closePopover()
        onToggleView()
      }}
    >
      <div className="liveCorner liveTL">{layoutName}</div>
      <div className="liveCorner liveTR">edo {edo} off {pitchOffset}</div>
      <div className="liveCorner liveBL">thr {live?.mode?.press_threshold?.toFixed?.(2) ?? ''}</div>
      <div className="liveCorner liveBR">
        at {live?.mode?.aftertouch || ''} oct {live?.mode?.octave_shift ?? 0}
      </div>

      <div className="liveMain" ref={mainWrapRef}>
        <div className="liveMainText" ref={mainTextRef}>
          {mainText || ' '}
        </div>
        <div className="liveIntervals">
          {intervalLines.map((it) => {
            const title = `${it.rootName} (${it.rootPc})`
            const best = it.bestName
            const moreCount = Math.max(0, it.allNames.length - (best ? 1 : 0))
            const showMore = view !== 'intervals' && moreCount > 0

            if (view === 'intervals') {
              return (
                <div key={it.rootPc} className="liveIntervalsLine liveChordBlock">
                  <div className="liveChordHeader">
                    {title}: {it.deltaText}
                  </div>
                  {it.allNames.map((n, i) => (
                    <div key={i} className="liveChordName">
                      {n}
                    </div>
                  ))}
                </div>
              )
            }

            return (
              <div key={it.rootPc} className="liveIntervalsLine liveChordLine">
                <span className="liveChordHeader">
                  {title}: {it.deltaText}
                </span>
                {best ? <span className="liveChordBest"> - {best}</span> : null}
                {showMore ? (
                  <button
                    className="liveMoreBtn"
                    type="button"
                    onPointerUp={(ev) => {
                      ev.preventDefault()
                      ev.stopPropagation()
                      openPopover(`${title} alternatives`, it.allNames)
                    }}
                  >
                    (+{moreCount})
                  </button>
                ) : null}
              </div>
            )
          })}
        </div>
        <div className="liveHint">tap to change view: {view}</div>
      </div>

      {namesPopover.open ? (
        <div
          className="livePopover"
          onPointerUp={(e) => {
            e.preventDefault()
            e.stopPropagation()
            closePopover()
          }}
        >
          <div className="livePopoverTitle">{namesPopover.title}</div>
          <div className="livePopoverList">
            {namesPopover.names.map((n, i) => (
              <div key={i} className="livePopoverRow">
                {n}
              </div>
            ))}
          </div>
          <div className="livePopoverHint">tap to close</div>
        </div>
      ) : null}
    </div>
  )
}
