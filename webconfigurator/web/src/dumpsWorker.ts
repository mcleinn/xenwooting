type Series = { id: string; xs: number[]; ys: number[]; head: number }

type State = {
  loadedUrl: string | null
  seriesById: Map<string, Series>
  xMin: number
  xMax: number
  tailMs: number
}

const st: State = {
  loadedUrl: null,
  seriesById: new Map(),
  xMin: 0,
  xMax: 0,
  tailMs: 0,
}

function parseTotalBytesFromContentRange(v: string | null): number | null {
  const s = String(v || '')
  const m = s.match(/\/(\d+)\s*$/)
  if (!m) return null
  const n = Number.parseInt(m[1] || '', 10)
  return Number.isFinite(n) ? n : null
}

function lowerBound(xs: number[], x: number) {
  let lo = 0
  let hi = xs.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if (xs[mid] < x) lo = mid + 1
    else hi = mid
  }
  return lo
}

function upperBound(xs: number[], x: number) {
  let lo = 0
  let hi = xs.length
  while (lo < hi) {
    const mid = (lo + hi) >> 1
    if (xs[mid] <= x) lo = mid + 1
    else hi = mid
  }
  return lo
}

function downsampleBucketLast(xs: number[], ys: number[], xStarts: number[], xEnds: number[]) {
  const out: Array<number | null> = new Array(xStarts.length).fill(null)
  let i = 0
  for (let b = 0; b < xStarts.length; b++) {
    const x0 = xStarts[b]
    const x1 = xEnds[b]
    // advance to first >= x0
    while (i < xs.length && xs[i] < x0) i++
    let last: number | null = null
    while (i < xs.length && xs[i] < x1) {
      last = ys[i]
      i++
    }
    out[b] = last
  }
  return out
}

async function loadCaptureCsv(url: string, opts: { tailBytes: number; wantedIds: Set<string> | null }) {
  const headers: Record<string, string> = {}
  if (opts.tailBytes > 0) headers.Range = `bytes=-${opts.tailBytes}`

  const res = await fetch(url, { headers })
  if (!res.ok) throw new Error(`fetch failed: ${res.status}`)
  if (!res.body) throw new Error('no body')

  st.seriesById.clear()
  st.loadedUrl = url
  st.xMin = 0
  st.xMax = 0

  const reader = res.body.getReader()
  const dec = new TextDecoder('utf-8')
  let buf = ''
  // capture.csv has a one-line header; if we read a tail range it won't be present.
  let isFirstLine = opts.tailBytes <= 0
  let skipPartialFirstLine = opts.tailBytes > 0

  const totalBytes =
    parseTotalBytesFromContentRange(res.headers.get('content-range')) ??
    (Number.parseInt(String(res.headers.get('content-length') || ''), 10) || 0)
  let bytesRead = 0
  let lastProgressAt = 0

  for (;;) {
    const { value, done } = await reader.read()
    if (done) break
    bytesRead += value?.byteLength || 0
    buf += dec.decode(value, { stream: true })

    const now = Date.now()
    if (now - lastProgressAt > 60) {
      lastProgressAt = now
      self.postMessage({ type: 'progress', phase: 'capture', loadedBytes: bytesRead, totalBytes })
    }

    for (;;) {
      const nl = buf.indexOf('\n')
      if (nl < 0) break
      const line = buf.slice(0, nl).trimEnd()
      buf = buf.slice(nl + 1)
      if (!line) continue

      if (skipPartialFirstLine) {
        skipPartialFirstLine = false
        // Range reads often start mid-line; drop the first fragment.
        continue
      }

      if (isFirstLine) {
        isFirstLine = false
        continue
      }

      // t_us,device_id,hid,analog,analog_q
      const parts = line.split(',')
      if (parts.length < 4) continue
      const tUs = Number.parseInt(parts[0] || '', 10)
      const dev = parts[1] || ''
      const hid = (parts[2] || '').replace(/^"|"$/g, '')
      const analog = Number.parseFloat(parts[3] || '')
      if (!Number.isFinite(tUs) || !Number.isFinite(analog)) continue
      const x = tUs / 1000.0
      const id = `${dev}:${hid}`
      if (opts.wantedIds && !opts.wantedIds.has(id)) continue
      let s = st.seriesById.get(id)
      if (!s) {
        s = { id, xs: [], ys: [], head: 0 }
        st.seriesById.set(id, s)
      }
      s.xs.push(x)
      s.ys.push(analog)
      if (x > st.xMax) st.xMax = x

      if (st.tailMs > 0 && st.xMax > st.tailMs) {
        const cutoff = st.xMax - st.tailMs
        while (s.head < s.xs.length && s.xs[s.head] < cutoff) s.head++
        if (s.head > 20000 && s.head > (s.xs.length >> 1)) {
          s.xs = s.xs.slice(s.head)
          s.ys = s.ys.slice(s.head)
          s.head = 0
        }
      }
    }
  }

  // flush last line
  buf = buf.trim()
  if (buf && !isFirstLine && !skipPartialFirstLine) {
    const parts = buf.split(',')
    if (parts.length >= 4) {
      const tUs = Number.parseInt(parts[0] || '', 10)
      const dev = parts[1] || ''
      const hid = (parts[2] || '').replace(/^"|"$/g, '')
      const analog = Number.parseFloat(parts[3] || '')
      if (Number.isFinite(tUs) && Number.isFinite(analog)) {
        const x = tUs / 1000.0
        const id = `${dev}:${hid}`
        if (!opts.wantedIds || opts.wantedIds.has(id)) {
          let s = st.seriesById.get(id)
          if (!s) {
            s = { id, xs: [], ys: [], head: 0 }
            st.seriesById.set(id, s)
          }
          s.xs.push(x)
          s.ys.push(analog)
          if (x > st.xMax) st.xMax = x

          if (st.tailMs > 0 && st.xMax > st.tailMs) {
            const cutoff = st.xMax - st.tailMs
            while (s.head < s.xs.length && s.xs[s.head] < cutoff) s.head++
            if (s.head > 20000 && s.head > (s.xs.length >> 1)) {
              s.xs = s.xs.slice(s.head)
              s.ys = s.ys.slice(s.head)
              s.head = 0
            }
          }
        }
      }
    }
  }

  if (st.tailMs > 0) {
    st.xMin = Math.max(0, st.xMax - st.tailMs)
    for (const s of st.seriesById.values()) {
      if (s.head > 0) {
        s.xs = s.xs.slice(s.head)
        s.ys = s.ys.slice(s.head)
        s.head = 0
      }
    }
  } else {
    st.xMin = 0
  }
}

self.onmessage = async (ev: MessageEvent) => {
  const msg = ev.data || {}
  try {
    if (msg.type === 'loadCapture') {
      const url = String(msg.url || '')
      if (!url) throw new Error('missing url')
      st.tailMs = Number(msg.tailMs || 0)
      const tailBytes = Math.max(0, Number(msg.tailBytes || 0))
      const wantedIdsArr = Array.isArray(msg.wantedIds) ? msg.wantedIds.map((x: any) => String(x || '')).filter(Boolean) : []
      const wantedIds: Set<string> | null = wantedIdsArr.length ? new Set<string>(wantedIdsArr) : null
      await loadCaptureCsv(url, { tailBytes, wantedIds })
      self.postMessage({ type: 'progress', phase: 'capture', loadedBytes: 1, totalBytes: 1 })
      self.postMessage({ type: 'loaded', seriesIds: Array.from(st.seriesById.keys()), xMin: st.xMin, xMax: st.xMax })
      return
    }

    if (msg.type === 'view') {
      const xMin = Number(msg.xMin)
      const xMax = Number(msg.xMax)
      const buckets = Math.max(10, Math.min(6000, Number(msg.buckets || 1400)))

      const idsRaw = Array.isArray(msg.ids) ? msg.ids.map((x: any) => String(x || '')).filter(Boolean) : null
      const ids = idsRaw && idsRaw.length ? idsRaw : null

      const span = Math.max(0.0001, xMax - xMin)
      const xStarts: number[] = []
      const xEnds: number[] = []
      const xAxis: number[] = []
      for (let b = 0; b < buckets; b++) {
        const a = xMin + (span * b) / buckets
        const z = xMin + (span * (b + 1)) / buckets
        xStarts.push(a)
        xEnds.push(z)
        xAxis.push((a + z) * 0.5)
      }

      const out = [] as Array<{ id: string; data: Array<number | null> }>
      const it = ids ? ids : Array.from(st.seriesById.keys())
      for (const id of it) {
        const s = st.seriesById.get(id)
        if (!s) continue
        // Only compute within range slice for speed.
        const i0 = lowerBound(s.xs, xMin)
        const i1 = upperBound(s.xs, xMax)
        const xs = s.xs.slice(i0, i1)
        const ys = s.ys.slice(i0, i1)
        out.push({ id: s.id, data: downsampleBucketLast(xs, ys, xStarts, xEnds) })
      }

      self.postMessage({ type: 'viewData', xMin, xMax, xAxis, series: out })
      return
    }
  } catch (e: any) {
    self.postMessage({ type: 'error', error: String(e?.message || e) })
  }
}
