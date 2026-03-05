export type LtnCell = { note: number; chan: number; col: string }

export type LtnData = {
  // LTN has boards numbered 0..4 (combined). Each board has Key_0..Key_55.
  boards: Record<number, Array<LtnCell | null>>
}

const CELL_COUNT = 56
const DEFAULT_COL = 'FFFFFF'

export function parseLtnText(text: string): LtnData {
  /** @type {Record<number, Record<number, Partial<LtnCell>>>} */
  const tmp: Record<number, Record<number, Partial<LtnCell>>> = {}
  /** @type {Record<number, Array<LtnCell | null>>} */
  const boards: Record<number, Array<LtnCell | null>> = {}

  let section: number | null = null

  for (const rawLine of text.split(/\r?\n/)) {
    const line = rawLine.trim()
    if (!line) continue
    if (line.startsWith(';') || line.startsWith('#')) continue

    const mSection = line.match(/^\[(.+)]$/)
    if (mSection) {
      const name = mSection[1]
      const m = name.match(/^Board(\d+)$/)
      if (m) {
        const b = Number.parseInt(m[1] || '', 10)
        section = Number.isFinite(b) ? b : null
      } else {
        section = null
      }
      continue
    }

    if (section === null) continue
    const eq = line.indexOf('=')
    if (eq === -1) continue

    const k = line.slice(0, eq).trim()
    const v = line.slice(eq + 1).trim()
    const mKey = k.match(/^(Key|Chan|Col)_(\d+)$/)
    if (!mKey) continue
    const kind = mKey[1]
    const idx = Number.parseInt(mKey[2] || '', 10)
    if (!Number.isFinite(idx) || idx < 0 || idx >= CELL_COUNT) continue

    tmp[section] ||= {}
    tmp[section][idx] ||= {}

    if (kind === 'Key') {
      const note = Number.parseInt(v, 10)
      if (Number.isFinite(note)) tmp[section][idx].note = note
    } else if (kind === 'Chan') {
      const chan = Number.parseInt(v, 10)
      if (Number.isFinite(chan)) tmp[section][idx].chan = chan
    } else if (kind === 'Col') {
      // Accept both RRGGBB and AARRGGBB (ignore alpha).
      const raw = v.replace(/^#/, '').trim().toUpperCase()
      if (/^[0-9A-F]{6}$/.test(raw)) {
        tmp[section][idx].col = raw
      } else if (/^[0-9A-F]{8}$/.test(raw)) {
        tmp[section][idx].col = raw.slice(2)
      } else if (raw.length >= 6 && /^[0-9A-F]+$/.test(raw)) {
        // Fall back to "last 6" behavior for other hex-like strings.
        tmp[section][idx].col = raw.slice(-6)
      }
    }
  }

  for (const [bStr, byIdx] of Object.entries(tmp)) {
    const b = Number.parseInt(bStr, 10)
    if (!Number.isFinite(b)) continue
    const arr: Array<LtnCell | null> = Array.from({ length: CELL_COUNT }, () => null)
    for (const [idxStr, cell0] of Object.entries(byIdx)) {
      const idx = Number.parseInt(idxStr, 10)
      if (!Number.isFinite(idx) || idx < 0 || idx >= CELL_COUNT) continue
      if (cell0.note === undefined || cell0.chan === undefined) continue
      arr[idx] = {
        note: cell0.note | 0,
        chan: cell0.chan | 0,
        col: cell0.col || DEFAULT_COL,
      }
    }
    boards[b] = arr
  }

  return { boards }
}
