import fs from 'node:fs/promises'

// Scala chord name DB support.
//
// We support two sources:
// 1) Exact EDO step-pattern chords under <SCALA_SCALE_DEF 2^(1/N)> blocks.
// 2) Tuning-independent chord templates (ratios / cents / 12-EDO step patterns),
//    projected into any EDO to provide "close" names when an EDO block lacks them.

const APPROX_MAX_ERR_CENTS = 15
const APPROX_SHOW_ERR_OVER_CENTS = 3

function mod(n, m) {
  const x = n % m
  return x < 0 ? x + m : x
}

function normPc(edo, pc) {
  const n = ((pc % edo) + edo) % edo
  return n | 0
}

function uniqSorted(nums) {
  const s = new Set()
  for (const n of nums) s.add(n)
  return Array.from(s.values()).sort((a, b) => a - b)
}

function stepPatternFromRelPcs(rel) {
  if (!rel || rel.length < 2) return ''
  const steps = []
  for (let i = 1; i < rel.length; i++) steps.push(rel[i] - rel[i - 1])
  return steps.join('-')
}

function stepPatternFromPcs(edo, rootPc, pcs) {
  const rel = []
  for (const pc of pcs) rel.push(normPc(edo, pc - rootPc))
  const uniq = uniqSorted(rel)
  if (uniq.length === 0) return { rel: [], pattern: '' }
  if (uniq[0] !== 0) return { rel: uniq, pattern: '' }
  if (uniq.length === 1) return { rel: uniq, pattern: '' }
  return { rel: uniq, pattern: stepPatternFromRelPcs(uniq) }
}

function parseRatioToken(tok) {
  const s = tok.trim()
  if (!s) return null
  const m = s.match(/^\(?([0-9]+)(?:\/([0-9]+))?\)?$/)
  if (!m) return null
  const a = Number.parseInt(m[1], 10)
  if (!Number.isFinite(a) || a <= 0) return null
  if (!m[2]) return { num: a, den: 1 }
  const b = Number.parseInt(m[2], 10)
  if (!Number.isFinite(b) || b <= 0) return null
  return { num: a, den: b }
}

function ratioToCents(r) {
  return 1200 * Math.log2(r)
}

function parseChordTemplateLine(raw) {
  // Returns { cents: number[], name: string } or null.
  const line = raw.trim()
  if (!line || line.startsWith('!')) return null
  // skip directives
  if (line.startsWith('<')) return null

  // EDO step patterns are handled elsewhere.
  if (line.match(/^\d+(?:-\d+)+\s+/)) return null

  // Absolute ratio list like 4:5:6 or 1:2:3:...
  if (line.includes(':')) {
    const m = line.match(/^([^\s]+)\s+(.+)$/)
    if (!m) return null
    const def = m[1]
    let name = m[2].trim()
    if (name.startsWith('=')) name = name.slice(1).trim()
    const toks = def.replace(/^\(|\)$/g, '').split(':')
    const rs = toks.map(parseRatioToken).filter(Boolean)
    if (rs.length < 2) return null
    const root = rs[0].num / rs[0].den
    const cents = [0]
    for (let i = 1; i < rs.length; i++) {
      const r = (rs[i].num / rs[i].den) / root
      cents.push(mod(ratioToCents(r), 1200))
    }
    return { cents: uniqSorted(cents), name }
  }

  // Relative intervals in cents or ratio, separated by spaces.
  // Example: 50.0 50.0 400.0  <name>
  // Example: 28/27 36/35 5/4 <name>
  const m = line.match(/^(.+?)\s{2,}(.+)$/)
  if (!m) return null
  const def = m[1].trim()
  let name = m[2].trim()
  if (name.startsWith('=')) name = name.slice(1).trim()
  const parts = def.split(/\s+/).filter(Boolean)
  if (parts.length < 2) return null

  // If any token looks like cents (has '.'), treat all as cents.
  const hasDot = parts.some((p) => p.includes('.'))
  const cents = [0]
  if (hasDot) {
    let acc = 0
    for (const p of parts) {
      const v = Number.parseFloat(p)
      if (!Number.isFinite(v)) return null
      acc += v
      cents.push(mod(acc, 1200))
    }
  } else {
    // Treat as relative ratios.
    let acc = 1
    for (const p of parts) {
      const r = parseRatioToken(p)
      if (!r) return null
      acc *= r.num / r.den
      cents.push(mod(ratioToCents(acc), 1200))
    }
  }
  return { cents: uniqSorted(cents), name }
}

function projectTemplateToEdo(centsList, edo) {
  const stepCents = 1200 / edo
  const pcs = []
  let maxAbsErr = 0
  for (const c of centsList) {
    const cents = mod(c, 1200)
    const k = Math.round(cents / stepCents)
    const realized = k * stepCents
    const err = realized - cents
    maxAbsErr = Math.max(maxAbsErr, Math.abs(err))
    pcs.push(normPc(edo, k))
  }
  const rel = uniqSorted(pcs)
  // Ensure root 0 exists.
  if (rel.length === 0 || rel[0] !== 0) rel.unshift(0)
  const pattern = stepPatternFromRelPcs(rel)
  return { rel, pattern, maxAbsErrCents: maxAbsErr }
}

function withErrSuffix(name, maxAbsErrCents) {
  if (!Number.isFinite(maxAbsErrCents)) return name
  if (maxAbsErrCents <= APPROX_SHOW_ERR_OVER_CENTS) return name
  const v = Math.round(maxAbsErrCents)
  return `${name} ~${v}c`
}

export async function loadScalaChordNamesDb(filePath) {
  const text = await fs.readFile(filePath, 'utf8')
  const byEdoExact = new Map() // edo -> Map(pattern -> string[])
  const templates = [] // { name, cents:number[] }

  let edo = null
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim()
    if (!line || line.startsWith('!')) continue

    const mEdo = line.match(/^<SCALA_SCALE_DEF\s+2\^\(1\/(\d+)\)>$/)
    if (mEdo) {
      edo = Number.parseInt(mEdo[1], 10)
      if (!Number.isFinite(edo) || edo <= 0) edo = null
      if (edo !== null && !byEdoExact.has(edo)) byEdoExact.set(edo, new Map())
      continue
    }

    // Exact EDO patterns under a scale def.
    if (edo !== null) {
      const m = line.match(/^([0-9]+(?:-[0-9]+)+)\s+(.+)$/)
      if (m) {
        const pattern = m[1]
        let name = m[2].trim()
        if (name.startsWith('=')) name = name.slice(1).trim()
        if (name) {
          const patterns = byEdoExact.get(edo)
          if (!patterns.has(pattern)) patterns.set(pattern, [])
          const names = patterns.get(pattern)
          if (!names.includes(name)) names.push(name)
        }
        continue
      }
    }

    // Templates (ratios/cents). These apply regardless of EDO.
    const tpl = parseChordTemplateLine(raw)
    if (tpl && tpl.cents && tpl.cents.length >= 2 && tpl.name) {
      templates.push(tpl)
    }
  }

  const approxByEdo = new Map() // edo -> Map(pattern -> {names:string[]})

  function ensureApproxForEdo(targetEdo) {
    if (approxByEdo.has(targetEdo)) return
    const m = new Map()
    for (const tpl of templates) {
      const proj = projectTemplateToEdo(tpl.cents, targetEdo)
      if (!proj.pattern) continue
      if (proj.maxAbsErrCents > APPROX_MAX_ERR_CENTS) continue
      const n = withErrSuffix(tpl.name, proj.maxAbsErrCents)
      if (!m.has(proj.pattern)) m.set(proj.pattern, [])
      const arr = m.get(proj.pattern)
      if (!arr.includes(n)) arr.push(n)
    }
    approxByEdo.set(targetEdo, m)
  }

  // Precompute approx maps for all EDOs that exist in the file.
  for (const e of byEdoExact.keys()) ensureApproxForEdo(e)

  return { byEdoExact, approxByEdo, ensureApproxForEdo }
}

export function findChordNames(db, edo, pitchClasses) {
  if (!db || !db.byEdoExact) return []
  db.ensureApproxForEdo?.(edo)
  const patternsExact = db.byEdoExact.get(edo) || new Map()
  const patternsApprox = db.approxByEdo?.get(edo) || new Map()

  const pcs = []
  for (const pc of pitchClasses || []) {
    const n = Number.parseInt(String(pc), 10)
    if (!Number.isFinite(n)) continue
    pcs.push(normPc(edo, n | 0))
  }
  const uniq = uniqSorted(pcs)
  if (uniq.length === 0) return []

  const out = []
  for (const rootPc of uniq) {
    const { pattern, rel } = stepPatternFromPcs(edo, rootPc, uniq)
    const names0 = pattern ? patternsExact.get(pattern) || [] : []
    const names1 = pattern ? patternsApprox.get(pattern) || [] : []
    // Keep exact names first; fill gaps with approximations.
    const names = [...names0]
    for (const n of names1) {
      if (!names.includes(n)) names.push(n)
    }
    out.push({ rootPc, rel, pattern, names })
  }
  return out
}
