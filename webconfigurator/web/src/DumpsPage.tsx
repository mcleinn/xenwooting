import { useEffect, useMemo, useRef, useState } from 'react'
import './DumpsPage.css'
import { fetchDumpsList, fetchDumpFileText, type DumpBoard, type DumpEntry } from './api'
import ReactECharts from 'echarts-for-react'

function apiUrl(path: string) {
  return new URL(path.replace(/^\//, ''), `${window.location.origin}/wtn/`).toString()
}

type Session = {
  tsMs: number
  capture?: DumpEntry
  dump?: DumpEntry
}

type ParsedCfg = {
  thr?: number
  thrUp?: number
  thrAt?: number
  hyst?: number
}

type DumpEvent = {
  xMs: number
  event: string
  kind: string
  deviceId: string
  hid: string
  analog?: number
  ch?: number
  note?: number
  vel?: number
  pressure?: number
}

type CurvePoint = { x: number; last: number; peak?: number }

function parseCfg(txt: string): ParsedCfg {
  const out: ParsedCfg = {}
  const line = (txt || '').split('\n').find((l) => l.startsWith('CFG ')) || ''
  const mThr = line.match(/\bthr=([0-9.]+)/)
  const mThrUp = line.match(/\bthr_up=([0-9.]+)/)
  const mThrAt = line.match(/\bthr_at=([0-9.]+)/)
  const mH = line.match(/\bhyst=([0-9.]+)/)
  if (mThr) out.thr = Number.parseFloat(mThr[1])
  if (mThrUp) out.thrUp = Number.parseFloat(mThrUp[1])
  if (mThrAt) out.thrAt = Number.parseFloat(mThrAt[1])
  if (mH) out.hyst = Number.parseFloat(mH[1])
  return out
}

function hashColor(s: string) {
  // Deterministic bright-ish palette generator.
  let h = 2166136261
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  const hue = (h >>> 0) % 360
  const sat = 72
  const lit = 58
  return `hsl(${hue} ${sat}% ${lit}%)`
}

function parseCsvRow(line: string) {
  // Simple CSV split (we only generate simple CSV without embedded commas except quoted HID).
  // HID field may be quoted; we just strip wrapping quotes later.
  return line.split(',')
}

function parseDumpCurves(dumpCsv: string, preferCapMs: boolean) {
  const lines = (dumpCsv || '').split('\n').filter(Boolean)
  if (!lines.length) return { pts: new Map<string, CurvePoint[]>(), xMin: 0, xMax: 1 }
  const header = parseCsvRow(lines[0])
  const idx = new Map<string, number>()
  for (let i = 0; i < header.length; i++) idx.set(header[i], i)

  const gi = (name: string) => idx.get(name)
  const iTms = gi('t_ms')
  const iCap = gi('t_cap_ms')
  const iEvent = gi('event')
  const iDev = gi('device_id')
  const iHid = gi('hid')
  const iLast = gi('last')
  const iPeak = gi('peak')

  const pts = new Map<string, CurvePoint[]>()
  let xMin = Infinity
  let xMax = -Infinity

  for (let li = 1; li < lines.length; li++) {
    const row = parseCsvRow(lines[li])
    const ev = iEvent !== undefined ? (row[iEvent] || '') : ''
    if (ev !== 'NOTEON_TICK') continue

    const xRaw = preferCapMs && iCap !== undefined ? row[iCap] : iTms !== undefined ? row[iTms] : ''
    const x = Number.parseFloat(String(xRaw || ''))
    if (!Number.isFinite(x)) continue
    const dev = iDev !== undefined ? String(row[iDev] || '') : ''
    const hid = iHid !== undefined ? String(row[iHid] || '') : ''
    const id = `${dev}:${hid.replace(/^"|"$/g, '')}`

    const yLast = iLast !== undefined ? Number.parseFloat(String(row[iLast] || '')) : NaN
    const yPeak = iPeak !== undefined ? Number.parseFloat(String(row[iPeak] || '')) : NaN

    if (Number.isFinite(yLast)) {
      const a = pts.get(id) || []
      const cp: CurvePoint = { x, last: yLast }
      if (Number.isFinite(yPeak)) cp.peak = yPeak
      a.push(cp)
      pts.set(id, a)
      xMin = Math.min(xMin, x)
      xMax = Math.max(xMax, x)
    }
  }

  if (!Number.isFinite(xMin) || !Number.isFinite(xMax) || xMax <= xMin) {
    xMin = 0
    xMax = 1
  }
  return { pts, xMin, xMax }
}

function downsampleBucketLast(pts: CurvePoint[], x0: number, x1: number, buckets: number) {
  if (!pts.length) return [] as Array<[number, number, number?]>
  const span = Math.max(0.0001, x1 - x0)
  const out: Array<[number, number, number?]> = []
  pts.sort((a, b) => a.x - b.x)
  let i = 0
  for (let b = 0; b < buckets; b++) {
    const a = x0 + (span * b) / buckets
    const z = x0 + (span * (b + 1)) / buckets
    const mid = (a + z) * 0.5
    while (i < pts.length && pts[i].x < a) i++
    let last: CurvePoint | null = null
    while (i < pts.length && pts[i].x < z) {
      last = pts[i]
      i++
    }
    if (last !== null) out.push([mid, last.last, last.peak])
  }
  return out
}

function parseDumpEvents(dumpCsv: string, preferCapMs: boolean): DumpEvent[] {
  const lines = (dumpCsv || '').split('\n').filter(Boolean)
  if (!lines.length) return []
  const header = parseCsvRow(lines[0])
  const idx = new Map<string, number>()
  for (let i = 0; i < header.length; i++) idx.set(header[i], i)

  const gi = (name: string) => idx.get(name)
  const iTms = gi('t_ms')
  const iCap = gi('t_cap_ms')
  const iEvent = gi('event')
  const iKind = gi('kind')
  const iDev = gi('device_id')
  const iHid = gi('hid')
  const iCh = gi('ch')
  const iNote = gi('note')
  const iVel = gi('vel')
  const iPressure = gi('pressure')
  const iAnalog = gi('analog')

  const out: DumpEvent[] = []
  for (let li = 1; li < lines.length; li++) {
    const row = parseCsvRow(lines[li])
    const ev = iEvent !== undefined ? (row[iEvent] || '') : ''
    const kind = iKind !== undefined ? (row[iKind] || '') : ''
    if (!ev) continue

    const xRaw = preferCapMs && iCap !== undefined ? row[iCap] : iTms !== undefined ? row[iTms] : ''
    const xMs = Number.parseFloat(String(xRaw || ''))
    if (!Number.isFinite(xMs)) continue
    const deviceId = iDev !== undefined ? String(row[iDev] || '') : ''
    const hid = iHid !== undefined ? String(row[iHid] || '') : ''
    const hid2 = hid.replace(/^"|"$/g, '')
    const e: DumpEvent = { xMs, event: ev, kind, deviceId, hid: hid2 }
    if (iAnalog !== undefined && row[iAnalog]) {
      const a = Number.parseFloat(row[iAnalog])
      if (Number.isFinite(a)) e.analog = a
    }
    if (iCh !== undefined && row[iCh]) e.ch = Number.parseInt(row[iCh], 10)
    if (iNote !== undefined && row[iNote]) e.note = Number.parseInt(row[iNote], 10)
    if (iVel !== undefined && row[iVel]) e.vel = Number.parseInt(row[iVel], 10)
    if (iPressure !== undefined && row[iPressure]) e.pressure = Number.parseInt(row[iPressure], 10)
    out.push(e)
  }
  return out
}

// (legacy helper removed; ECharts uses [x,y] pairs directly)

export default function DumpsPage() {
  const [sessions, setSessions] = useState<Session[]>([])
  const [selTs, setSelTs] = useState<number | null>(null)
  const [status, setStatus] = useState<string>('')
  const [err, setErr] = useState<string>('')

  const [captureTxt, setCaptureTxt] = useState<string>('')
  const [dumpTxt, setDumpTxt] = useState<string>('')
  const [cfg, setCfg] = useState<ParsedCfg>({})

  const workerRef = useRef<Worker | null>(null)
  const chartRef = useRef<any>(null)
  const [seriesIds, setSeriesIds] = useState<string[]>([])
  const [xMin, setXMin] = useState<number>(0)
  const [xMax, setXMax] = useState<number>(1)
  const [viewPairsById, setViewPairsById] = useState<Map<string, Array<[number, number, number?]>>>(new Map())
  const [events, setEvents] = useState<DumpEvent[]>([])
  const [cursorX, setCursorX] = useState<number | null>(null)
  const [_atKeys, setAtKeys] = useState<string[]>([])
  const [boards, setBoards] = useState<DumpBoard[]>([])
  const [eventInfo, setEventInfo] = useState<string>('')

  const lastSessionTsRef = useRef<number | null>(null)

  const buckets = 1800

  useEffect(() => {
    const w = new Worker(new URL('./dumpsWorker.ts', import.meta.url), { type: 'module' })
    workerRef.current = w
    w.onmessage = (ev: MessageEvent) => {
      const msg = ev.data || {}
      if (msg.type === 'error') {
        setErr(String(msg.error || 'worker error'))
        return
      }
      if (msg.type === 'loaded') {
        const ids = Array.isArray(msg.seriesIds) ? msg.seriesIds : []
        setSeriesIds(ids)
        setXMin(Number(msg.xMin || 0))
        setXMax(Number(msg.xMax || 0) || 1)
        // Request full-range view (downsampled); ECharts handles zoom/pan.
        w.postMessage({ type: 'view', xMin: Number(msg.xMin || 0), xMax: Number(msg.xMax || 0), buckets })
        return
      }
      if (msg.type === 'viewData') {
        const xa = Array.isArray(msg.xAxis) ? msg.xAxis : []
        const series = Array.isArray(msg.series) ? msg.series : []
        const m = new Map<string, Array<[number, number]>>()
        for (const s of series) {
          const id = String(s.id || '')
          const data = Array.isArray(s.data) ? s.data : []
          const pairs: Array<[number, number]> = []
          for (let i = 0; i < xa.length && i < data.length; i++) {
            const y = data[i]
            if (y === null || y === undefined) continue
            const yy = Number(y)
            if (!Number.isFinite(yy)) continue
            pairs.push([Number(xa[i]), yy])
          }
          if (pairs.length) m.set(id, pairs)
        }
        setViewPairsById(m)
        return
      }
    }
    return () => {
      w.terminate()
      workerRef.current = null
    }
  }, [])

  useEffect(() => {
    let cancelled = false
    setStatus('Loading dumps...')
    fetchDumpsList()
      .then((res) => {
        if (cancelled) return
        setBoards(Array.isArray(res.boards) ? res.boards : [])
        const byTs = new Map<number, Session>()
        for (const e of res.entries || []) {
          const tsMs = Number(e.tsMs)
          if (!Number.isFinite(tsMs)) continue
          const cur = byTs.get(tsMs) || { tsMs }
          if (e.kind === 'capture') cur.capture = e
          if (e.kind === 'dump') cur.dump = e
          byTs.set(tsMs, cur)
        }
        const list = Array.from(byTs.values()).sort((a, b) => b.tsMs - a.tsMs)
        setSessions(list)
        if (list.length && selTs === null) setSelTs(list[0].tsMs)
        setStatus('')
      })
      .catch((e) => {
        if (cancelled) return
        setErr(String(e?.message || e))
        setStatus('')
      })
    return () => {
      cancelled = true
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const boardByDeviceId = useMemo(() => {
    const m = new Map<string, number>()
    for (const b of boards) {
      const id = String((b as any)?.deviceId || '').trim()
      const n = Number((b as any)?.wtnBoard)
      if (!id || !Number.isFinite(n)) continue
      m.set(id, n)
    }
    return m
  }, [boards])

  const fmtTs = (tsMs: number) => {
    try {
      const d = new Date(tsMs)
      const pad = (n: number) => String(n).padStart(2, '0')
      return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
    } catch {
      return String(tsMs)
    }
  }

  const displayKey = (deviceId: string, hid: string) => {
    const dev = String(deviceId || '').trim()
    const b = boardByDeviceId.get(dev)
    const h = String(hid || '').trim()
    if (Number.isFinite(b as number)) return `${b}:${h}`
    return `${dev}:${h}`
  }

  const fmtDur = (ms: number) => {
    if (!Number.isFinite(ms) || ms < 0) return ''
    const total = Math.round(ms)
    const s = Math.floor(total / 1000)
    const rem = total % 1000
    if (s <= 0) return `${rem}ms`
    return `${s}s ${String(rem).padStart(3, '0')}ms`
  }

  const selected = useMemo(() => sessions.find((s) => s.tsMs === selTs) || null, [sessions, selTs])

  // Reset zoom + legend state when switching sessions.
  useEffect(() => {
    const ts = selected?.tsMs ?? null
    if (ts === null) return
    if (lastSessionTsRef.current === ts) return
    lastSessionTsRef.current = ts

    setCursorX(null)
    setEventInfo('')

    // Defer until chart instance has applied new option.
    setTimeout(() => {
      const inst = chartRef.current?.getEchartsInstance?.()
      if (!inst) return
      inst.dispatchAction({ type: 'dataZoom', dataZoomIndex: 0, start: 0, end: 100 })
      inst.dispatchAction({ type: 'dataZoom', dataZoomIndex: 1, start: 0, end: 100 })
      inst.dispatchAction({ type: 'legendAllSelect' })
      inst.dispatchAction({ type: 'legendUnSelect', name: 'thresholds' })
      inst.dispatchAction({ type: 'legendUnSelect', name: 'cursor' })
    }, 0)
  }, [selected?.tsMs])

  useEffect(() => {
    if (!selected) return
    setErr('')
    setStatus('Loading session...')
    setCaptureTxt('')
    setDumpTxt('')
    setCfg({})
    setEvents([])
    setCursorX(null)
    setEventInfo('')

    const ts = selected.tsMs
    const loadTxt = async () => {
      const capTxt = selected.capture?.hasTxt ? await fetchDumpFileText(ts, 'capture', 'txt') : ''
      const dTxt = selected.dump?.hasTxt ? await fetchDumpFileText(ts, 'dump', 'txt') : ''
      setCaptureTxt(capTxt)
      setDumpTxt(dTxt)
      setCfg(parseCfg(`${capTxt}\n${dTxt}`))
    }

    const loadDumpEvents = async () => {
      if (!selected.dump?.hasCsv) {
        setEvents([])
        return
      }
      const dumpCsv = await fetchDumpFileText(ts, 'dump', 'csv')
      const preferCap = Boolean(selected.capture?.hasCsv)
      setEvents(parseDumpEvents(dumpCsv, preferCap))
    }

    const loadChart = async () => {
      if (selected.capture?.hasCsv) {
        const url = apiUrl(`api/dumps/${encodeURIComponent(String(ts))}/capture.csv`)
        workerRef.current?.postMessage({ type: 'loadCapture', url })
      } else if (selected.dump?.hasCsv) {
        // Dump-only: render curves from NOTEON_TICK.last (and peak as secondary).
        const dumpCsv = await fetchDumpFileText(ts, 'dump', 'csv')
        const curves = parseDumpCurves(dumpCsv, false)
        const ids = Array.from(curves.pts.keys())
        setSeriesIds(ids)
        setXMin(curves.xMin)
        setXMax(curves.xMax)

        // Build initial view pairs.
        const x0 = curves.xMin
        const x1 = curves.xMax
        const m = new Map<string, Array<[number, number, number?]>>()
        for (const id of ids) {
          m.set(id, downsampleBucketLast(curves.pts.get(id) || [], x0, x1, buckets))
        }
        setViewPairsById(m)
      } else {
        setSeriesIds([])
        setViewPairsById(new Map())
      }
    }

    Promise.all([loadTxt(), loadDumpEvents(), loadChart()])
      .then(() => setStatus(''))
      .catch((e) => {
        setErr(String(e?.message || e))
        setStatus('')
      })
  }, [selected])

  // Aftertouch series are only shown during capture.
  useEffect(() => {
    if (!selected?.capture?.hasCsv) {
      setAtKeys([])
      return
    }
    const keys = new Set<string>()
    for (const e of events) {
      if (e.event !== 'AFTERTOUCH') continue
      const key = `${e.deviceId}:${e.hid}`
      keys.add(key)
    }
    setAtKeys(Array.from(keys.values()).map((k) => `AT:${k}`))
  }, [events, selected?.capture?.hasCsv])

  const chartOption = useMemo(() => {
    const evByKey = new Map<string, DumpEvent[]>()
    for (const e of events) {
      const key = `${e.deviceId}:${e.hid}`
      const arr = evByKey.get(key) || []
      arr.push(e)
      evByKey.set(key, arr)
    }

    const legendSelected: Record<string, boolean> = {}

    const series: any[] = []
    const edgePointsById = new Map<string, Array<any>>()
    const breaksById = new Map<string, Array<[number, null, null]>>()
    for (const e of events) {
      if (e.event !== 'EDGE') continue
      if (e.kind !== 'down' && e.kind !== 'up') continue
      if (!Number.isFinite(e.analog as number)) continue
      const id = `${e.deviceId}:${e.hid}`
      const arr = edgePointsById.get(id) || []
      const title = e.kind === 'down' ? 'DOWN' : 'UP'
      arr.push({
        value: [e.xMs, Number(e.analog), null],
        name: `${title} ${displayKey(e.deviceId, e.hid)}`,
        symbol: e.kind === 'down' ? 'circle' : 'emptyCircle',
        symbolSize: e.kind === 'down' ? 10 : 11,
        itemStyle: {
          color: e.kind === 'down' ? hashColor(id) : 'transparent',
          borderColor: hashColor(id),
          borderWidth: 2,
        },
        tooltip: {
          show: true,
          formatter: () => {
            const t = e.xMs.toFixed(1)
            return `<div style="font-size:12px"><b>${title} ${displayKey(e.deviceId, e.hid)}</b><div style="opacity:.85">t=${t}ms</div><div style="opacity:.85">analog=${Number(e.analog).toFixed(3)}</div></div>`
          },
        },
      })
      edgePointsById.set(id, arr)

      // Ensure the line breaks after UP so separate presses don't connect.
      if (e.kind === 'up') {
        const b = breaksById.get(id) || []
        b.push([e.xMs + 0.001, null, null])
        breaksById.set(id, b)
      }
    }

    for (const id of seriesIds) {
      const [dev, ...rest] = id.split(':')
      const hid = rest.join(':')
      const name = displayKey(dev, hid)
      const color = hashColor(id)
      const data = viewPairsById.get(id) || []

      const marks: any[] = []
      for (const ev of evByKey.get(id) || []) {
        const isEdge = ev.event === 'EDGE' && (ev.kind === 'down' || ev.kind === 'up')
        const evName = String(ev.event || '')
        const kindName = String(ev.kind || '')
        const isMidi =
          evName === 'MIDI' ||
          evName.startsWith('MIDI_') ||
          (evName.startsWith('MIDI') && (kindName.includes('noteon') || kindName.includes('noteoff')))
        if (!isEdge && !isMidi) continue

        let title = ''
        if (isEdge) {
          title = ev.kind === 'down' ? 'DOWN' : 'UP'
        } else {
          const on = kindName.includes('noteon') || evName.includes('NOTEON')
          const off = kindName.includes('noteoff') || evName.includes('NOTEOFF')
          title = on ? 'MIDI ON' : off ? 'MIDI OFF' : 'MIDI'
        }

        let detailStr = ''
        if (isMidi) {
          const ch = Number.isFinite(ev.ch as number) ? Number(ev.ch) : null
          const note = Number.isFinite(ev.note as number) ? Number(ev.note) : null
          const vel = Number.isFinite(ev.vel as number) ? Number(ev.vel) : null
          if (ch !== null && note !== null) {
            const v = vel !== null ? ` v${vel}` : ''
            detailStr = ` (${ch}-${note}${v})`
          }
        }

        marks.push({
          xAxis: ev.xMs,
          name: isEdge ? `${title} ${name}` : `${title}${detailStr}`,
          event: ev,
          label: {
            show: true,
            formatter: (p: any) => String(p?.data?.name || ''),
            position: isEdge && ev.kind === 'up' ? 'insideEndBottom' : 'insideEndTop',
            color: 'rgba(255,255,255,0.92)',
            fontSize: 11,
            padding: [2, 6, 2, 6],
            backgroundColor: 'rgba(0,0,0,0.45)',
            borderColor: 'rgba(255,255,255,0.18)',
            borderWidth: 1,
            borderRadius: 6,
          },
        })
      }

      series.push({
        name,
        type: 'line',
        showSymbol: false,
        data: [...data, ...(edgePointsById.get(id) || []), ...(breaksById.get(id) || [])].sort((a: any, b: any) => {
          const ax = Array.isArray(a) ? a[0] : Array.isArray(a?.value) ? a.value[0] : 0
          const bx = Array.isArray(b) ? b[0] : Array.isArray(b?.value) ? b.value[0] : 0
          return ax - bx
        }),
        lineStyle: { width: 1.6, color },
        emphasis: { disabled: true },
        blur: { lineStyle: { opacity: 1 } },
        legendHoverLink: false,
        hoverAnimation: false,
        markLine:
          marks.length > 0
            ? {
                symbol: 'none',
                lineStyle: { color, opacity: 0.5, width: 1, type: 'dashed' },
                label: {
                  show: true,
                  formatter: '{b}',
                },
                tooltip: {
                  show: true,
                  formatter: (p: any) => {
                    const d = p?.data || {}
                    const nm = String(d?.name || '')
                    const t = typeof d?.xAxis === 'number' ? d.xAxis.toFixed(1) : ''
                    return `<div style="font-size:12px"><b>${nm}</b><div style="opacity:.85">t=${t}ms</div></div>`
                  },
                },
                emphasis: { disabled: true },
                data: marks,
              }
            : undefined,
      })

      if (selected?.capture?.hasCsv) {
        // Aftertouch markers (scatter), only during capture sessions.
        const atPts: Array<[number, number]> = []
        for (const ev of evByKey.get(id) || []) {
          if (ev.event !== 'AFTERTOUCH') continue
          if (!Number.isFinite(ev.pressure as number)) continue
          atPts.push([ev.xMs, (ev.pressure as number) / 127.0])
        }
        if (atPts.length) {
          series.push({
            name: `AT ${name}`,
            type: 'scatter',
            data: atPts,
            symbolSize: 6,
            itemStyle: { color, opacity: 0.9 },
          })
        }
      }
    }

    const yLines: any[] = []
    if (Number.isFinite(cfg.thr as number)) yLines.push({ yAxis: cfg.thr, name: 'thr' })
    if (Number.isFinite(cfg.thrUp as number)) yLines.push({ yAxis: cfg.thrUp, name: 'thr_up' })
    if (Number.isFinite(cfg.thrAt as number)) yLines.push({ yAxis: cfg.thrAt, name: 'thr_at' })

    if (yLines.length) {
      series.push({
        name: 'thresholds',
        type: 'line',
        data: [],
        silent: true,
        markLine: {
          symbol: 'none',
          lineStyle: { color: 'rgba(255,255,255,0.35)', type: 'dashed', width: 1 },
          label: { show: true, color: 'rgba(255,255,255,0.65)', fontSize: 11 },
          data: yLines,
        },
      })
      legendSelected['thresholds'] = false
    }

    if (cursorX !== null) {
      series.push({
        name: 'cursor',
        type: 'line',
        data: [],
        silent: true,
        markLine: {
          symbol: 'none',
          lineStyle: { color: 'rgba(255,255,255,0.55)', type: 'dashed', width: 1 },
          label: { show: true, color: 'rgba(255,255,255,0.75)', fontSize: 11 },
          data: [{ xAxis: cursorX, name: `t=${cursorX.toFixed(1)}ms` }],
        },
      })
      legendSelected['cursor'] = false
    }

    return {
      animation: false,
      backgroundColor: 'transparent',
      grid: { left: 58, right: 18, top: 14, bottom: 92 },
      legend: {
        type: 'scroll',
        top: 'auto',
        bottom: 34,
        textStyle: { color: 'rgba(255,255,255,0.70)' },
        selected: legendSelected,
      },
      tooltip: {
        trigger: 'axis',
        axisPointer: { type: 'cross', triggerEmphasis: false },
        // Avoid ECharts default "dim other series" behavior on hover.
        confine: true,
        formatter: (params: any) => {
          const arr = Array.isArray(params) ? params : [params]
          if (!arr.length) return ''
          const x = arr[0]?.axisValue
          const head = typeof x === 'number' ? `t=${x.toFixed(1)}ms` : `t=${String(x)}`
          const lines: string[] = []
          for (const p of arr) {
            const nm = String(p?.seriesName || '')
            const v = p?.value
            let y: any = null
            let peak: any = null
            if (Array.isArray(v)) {
              y = v[1]
              peak = v.length >= 3 ? v[2] : null
            } else {
              y = v
            }
            if (typeof y !== 'number') continue
            const yStr = y.toFixed(3)
            const pkStr = typeof peak === 'number' ? ` <span style="opacity:.75">(peak ${peak.toFixed(3)})</span>` : ''
            lines.push(`<div><span style="opacity:.9">${nm}</span>: <b>${yStr}</b>${pkStr}</div>`)
          }
          return `<div style="font-size:12px"><div style="opacity:.85;margin-bottom:4px">${head}</div>${lines.join('')}</div>`
        },
      },
      xAxis: {
        type: 'value',
        name: 'ms',
        min: xMin,
        max: xMax,
        axisLine: { lineStyle: { color: 'rgba(255,255,255,0.35)' } },
        splitLine: { lineStyle: { color: 'rgba(255,255,255,0.10)' } },
      },
      yAxis: {
        type: 'value',
        min: 0,
        max: 1,
        axisLine: { lineStyle: { color: 'rgba(255,255,255,0.35)' } },
        splitLine: { lineStyle: { color: 'rgba(255,255,255,0.10)' } },
      },
      dataZoom: [
        { type: 'inside', xAxisIndex: 0, zoomOnMouseWheel: true, moveOnMouseWheel: true, filterMode: 'none' },
        {
          type: 'slider',
          xAxisIndex: 0,
          height: 26,
          bottom: 4,
          filterMode: 'none',
          textStyle: { color: 'rgba(255,255,255,0.65)' },
        },
      ],
      series,
    }
  }, [seriesIds, viewPairsById, events, cfg, cursorX, selected, boardByDeviceId, xMin, xMax])

  const renderTxt = (txt: string) => {
    const parts: any[] = []
    const re = /\b(\d+)ms\b/g
    let i = 0
    for (const line of String(txt || '').split('\n')) {
      let last = 0
      re.lastIndex = 0
      for (;;) {
        const m = re.exec(line)
        if (!m) break
        const a = m.index
        const b = m.index + m[0].length
        if (a > last) parts.push(<span key={`t${i++}`}>{line.slice(last, a)}</span>)
        const ms = Number.parseInt(m[1], 10)
        parts.push(
          <span
            key={`m${i++}`}
            style={{ textDecoration: 'underline', cursor: 'pointer' }}
            onClick={() => {
              if (!Number.isFinite(ms)) return
              setCursorX(ms)
              const inst = chartRef.current?.getEchartsInstance?.()
              if (inst) {
                const span = Math.min(5000, Math.max(400, (xMax - xMin) * 0.08))
                const a = Math.max(xMin, ms - span * 0.5)
                const z = Math.min(xMax, ms + span * 0.5)
                inst.dispatchAction({ type: 'dataZoom', dataZoomIndex: 0, startValue: a, endValue: z })
                inst.dispatchAction({ type: 'dataZoom', dataZoomIndex: 1, startValue: a, endValue: z })
              }
            }}
            title="Jump to time"
          >
            {m[0]}
          </span>,
        )
        last = b
      }
      if (last < line.length) parts.push(<span key={`t${i++}`}>{line.slice(last)}</span>)
      parts.push(<br key={`br${i++}`} />)
    }
    return parts
  }

  return (
    <div className="dpRoot">
      <div className="dpGrid">
        <div className="dpPanel dpLeft">
          <div className="dpHeader">
            <div>
              <div style={{ fontSize: 16, fontWeight: 650 }}>Dumps</div>
              <small className="dpMut">Newest first</small>
            </div>
            <small className="dpMut">/wtn/dumps</small>
          </div>
          <div className="dpListWrap">
            {err ? <div className="dpErr">{err}</div> : null}
            {status ? <div className="dpMut">{status}</div> : null}
            <div style={{ display: 'flex', flexDirection: 'column', gap: 8 }}>
              {sessions.map((s) => {
                const active = s.tsMs === selTs
                const hasCap = Boolean(s.capture?.hasCsv)
                const hasDump = Boolean(s.dump?.hasCsv)
                const label = fmtTs(s.tsMs)
                return (
                  <button
                    key={s.tsMs}
                    onClick={() => setSelTs(s.tsMs)}
                    style={{
                      textAlign: 'left',
                      padding: '10px 10px',
                      borderRadius: 12,
                      border: `1px solid ${active ? 'rgba(255,255,255,0.35)' : 'var(--stroke)'}`,
                      background: active ? 'rgba(255,255,255,0.10)' : 'rgba(0,0,0,0.12)',
                      color: 'var(--text)',
                      cursor: 'pointer',
                    }}
                  >
                    <div style={{ display: 'flex', justifyContent: 'space-between', gap: 10 }}>
                      <div style={{ fontWeight: 650 }}>{label}</div>
                      <div className="dpMut" style={{ fontSize: 12 }}>
                        {hasCap ? 'capture' : ''}
                        {hasCap && hasDump ? ' + ' : ''}
                        {hasDump ? 'dump' : ''}
                      </div>
                    </div>
                    <div className="dpMut" style={{ fontSize: 12, marginTop: 4 }}>
                      {s.tsMs} {hasCap ? ' · high-res capture' : ' · dump'}
                    </div>
                  </button>
                )
              })}
            </div>
          </div>
        </div>

        <div className="dpPanel dpMain">
          <div className="dpHeader">
            <div>
              <div style={{ fontSize: 16, fontWeight: 650 }}>Session</div>
              <small className="dpMut">
                {selected ? `${fmtTs(selected.tsMs)} (${selected.tsMs})` : 'none'}
                {selected?.capture?.hasCsv ? ' (capture + events)' : selected?.dump?.hasCsv ? ' (dump only)' : ''}
                {Number.isFinite(xMax - xMin) && xMax > xMin ? ` · ${fmtDur(xMax - xMin)}` : ''}
              </small>
            </div>
            <small className="dpMut"></small>
          </div>

          <div className="dpChartWrap">
            <div
              style={{ overflow: 'hidden' }}
            >
              {seriesIds.length ? (
                <ReactECharts
                  ref={chartRef}
                  option={chartOption}
                  style={{ height: 460, width: '100%' }}
                  notMerge={false}
                  lazyUpdate
                  onEvents={{
                    mouseover: (p: any) => {
                      if (p?.componentType === 'markLine') {
                        const d = p?.data || {}
                        const key = p?.seriesName ? ` (${p.seriesName})` : ''
                        const nm = String(d?.name || 'event')
                        setEventInfo(`${nm}${key}`)
                      }
                    },
                    mouseout: (p: any) => {
                      if (p?.componentType === 'markLine') {
                        setEventInfo('')
                      }
                    },
                    click: (p: any) => {
                      if (p?.componentType === 'markLine') {
                        const d = p?.data || {}
                        const key = p?.seriesName ? ` (${p.seriesName})` : ''
                        const nm = String(d?.name || 'event')
                        const t = typeof d?.xAxis === 'number' ? d.xAxis : (Array.isArray(p?.value) ? p.value[0] : null)
                        if (typeof t === 'number') setCursorX(t)
                        setEventInfo(`${nm}${key}`)
                        return
                      }
                      if (p?.value && Array.isArray(p.value) && Number.isFinite(p.value[0])) {
                        setCursorX(Number(p.value[0]))
                        setEventInfo('')
                      }
                    },
                  }}
                />
              ) : (
                <div className="dpMut">No CSV loaded for this timestamp.</div>
              )}
            </div>

            <div className="dpControls">
              <div className="dpControlsLeft">
                <button
                  onClick={() => {
                    setCursorX(null)
                    setEventInfo('')
                    const inst = chartRef.current?.getEchartsInstance?.()
                    if (inst) {
                      inst.dispatchAction({ type: 'dataZoom', dataZoomIndex: 0, start: 0, end: 100 })
                      inst.dispatchAction({ type: 'dataZoom', dataZoomIndex: 1, start: 0, end: 100 })

                      // Reset legend/series visibility.
                      inst.dispatchAction({ type: 'legendAllSelect' })
                      inst.dispatchAction({ type: 'legendUnSelect', name: 'thresholds' })
                      inst.dispatchAction({ type: 'legendUnSelect', name: 'cursor' })
                    }
                  }}
                  style={{
                    padding: '6px 10px',
                    borderRadius: 10,
                    border: '1px solid var(--stroke)',
                    background: 'rgba(0,0,0,0.14)',
                    color: 'var(--text)',
                    cursor: 'pointer',
                  }}
                >
                  Reset
                </button>
              </div>
              <div className="dpMut" style={{ fontSize: 12 }}>
                {selected?.capture?.hasCsv ? `series=${seriesIds.length}` : ''}
                {selected?.dump?.hasCsv ? ` events=${events.length}` : ''}
                {eventInfo ? (
                  <span className="dpMut" style={{ marginLeft: 10 }}>
                    {eventInfo}
                  </span>
                ) : null}
                <span className="dpMut" style={{ marginLeft: 10 }}>
                  drag/scroll to pan, wheel to zoom
                </span>
              </div>
            </div>
          </div>

          <div className="dpTxt">
            {captureTxt ? (
              <>
                <div className="dpMut" style={{ marginBottom: 6 }}>
                  capture.txt
                </div>
                <div>{renderTxt(captureTxt)}</div>
              </>
            ) : null}
            {dumpTxt ? (
              <>
                <div className="dpMut" style={{ margin: '10px 0 6px 0' }}>
                  dump.txt
                </div>
                <div>{renderTxt(dumpTxt)}</div>
              </>
            ) : null}
          </div>
        </div>
      </div>
    </div>
  )
}
