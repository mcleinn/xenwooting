import fs from 'node:fs/promises'

// Parse Scala chordnam.par and index EDO step-pattern chord names.
//
// We only support patterns under <SCALA_SCALE_DEF 2^(1/N)> blocks.
// Example patterns: 4-3-3, 3-4-3, 3-3-4

export async function loadScalaChordNamesDb(filePath) {
  const text = await fs.readFile(filePath, 'utf8')
  const byEdo = new Map() // edo -> Map(pattern -> string[])

  let edo = null
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim()
    if (!line || line.startsWith('!')) continue

    const mEdo = line.match(/^<SCALA_SCALE_DEF\s+2\^\(1\/(\d+)\)>$/)
    if (mEdo) {
      edo = Number.parseInt(mEdo[1], 10)
      if (!Number.isFinite(edo) || edo <= 0) edo = null
      if (edo !== null && !byEdo.has(edo)) byEdo.set(edo, new Map())
      continue
    }

    if (edo === null) continue

    // Step pattern lines begin with digits and contain '-' between steps.
    // The rest of the line is the name. If it begins with a digit, Scala uses '='.
    const m = line.match(/^([0-9]+(?:-[0-9]+)+)\s+(.+)$/)
    if (!m) continue
    const pattern = m[1]
    let name = m[2].trim()
    if (name.startsWith('=')) name = name.slice(1).trim()
    if (!name) continue

    const patterns = byEdo.get(edo)
    if (!patterns.has(pattern)) patterns.set(pattern, [])
    const names = patterns.get(pattern)
    if (!names.includes(name)) names.push(name)
  }

  return { byEdo }
}

function normPc(edo, pc) {
  const n = ((pc % edo) + edo) % edo
  return n | 0
}

function stepPatternFromPcs(edo, rootPc, pcs) {
  const rel = []
  for (const pc of pcs) {
    const d = normPc(edo, pc - rootPc)
    rel.push(d)
  }
  // Keep unique pitch classes.
  rel.sort((a, b) => a - b)
  const uniq = []
  for (const x of rel) {
    if (uniq.length === 0 || uniq[uniq.length - 1] !== x) uniq.push(x)
  }
  if (uniq.length === 0) return { rel: [], pattern: '' }
  if (uniq[0] !== 0) {
    // Root not present; treat as invalid.
    return { rel: uniq, pattern: '' }
  }
  if (uniq.length === 1) return { rel: uniq, pattern: '' }

  const steps = []
  for (let i = 1; i < uniq.length; i++) {
    steps.push(uniq[i] - uniq[i - 1])
  }
  return { rel: uniq, pattern: steps.join('-') }
}

export function findChordNames(db, edo, pitchClasses) {
  if (!db || !db.byEdo) return []
  const patterns = db.byEdo.get(edo)
  if (!patterns) return []

  const pcs = []
  for (const pc of pitchClasses || []) {
    if (!Number.isInteger(pc)) continue
    pcs.push(normPc(edo, pc))
  }
  pcs.sort((a, b) => a - b)
  const uniq = []
  for (const x of pcs) {
    if (uniq.length === 0 || uniq[uniq.length - 1] !== x) uniq.push(x)
  }
  if (uniq.length === 0) return []

  // Consider each chord tone as a possible root.
  const out = []
  for (const rootPc of uniq) {
    const { pattern, rel } = stepPatternFromPcs(edo, rootPc, uniq)
    const names = pattern ? patterns.get(pattern) || [] : []
    out.push({ rootPc, rel, pattern, names })
  }
  return out
}
