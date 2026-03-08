import fs from 'node:fs/promises'

// Scala chord name DB support.
//
// We support two sources:
// 1) Exact EDO step-pattern chords under <SCALA_SCALE_DEF 2^(1/N)> blocks.
// 2) Tuning-independent chord templates (ratios / cents / 12-EDO step patterns),
//    projected into any EDO to provide "close" names when an EDO block lacks them.

const APPROX_MAX_ERR_CENTS = 15
const APPROX_SHOW_ERR_OVER_CENTS = 3

// Keep scoring compatible with the live HUD sorting.
export function nameScore(name) {
  const s = String(name || '')
  const lower = s.toLowerCase()
  let score = 0

  // Prefer exact EDO12-embedded names when present.
  if (lower.includes('-edo12')) score -= 800

  // Prefer conventional major/minor triad names over overtone/undertone jargon.
  if (lower.includes('major triad')) score -= 1200
  if (lower.includes('minor triad')) score -= 1200
  if (lower.includes('overtone')) score += 500
  if (lower.includes('undertone')) score += 500

  // Prefer neutral-triad naming in 24-EDO contexts.
  if (lower.startsWith('neutral triad')) score -= 2200
  else if (lower.includes('neutral triad')) score -= 1600

  // Prefer exact / unqualified names over approximations with cents error.
  if (lower.match(/~\d+c\b/)) score += 220

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
  // (Length differences matter a lot in Scala where one entry can be multiple aliases.)
  score += Math.min(500, s.length * 2)
  if (s.length > 22) score += Math.min(800, (s.length - 22) * 6)

  // Penalize very "busy" names.
  score += (s.match(/[()"']/g) || []).length * 10
  const commaCount = (s.match(/,/g) || []).length
  score += commaCount * 40
  if (commaCount >= 2) score += 120
  return score
}

export function bestName(names) {
  if (!Array.isArray(names) || names.length === 0) return ''
  const sorted = [...names]
    .filter(Boolean)
    .sort((a, b) => nameScore(a) - nameScore(b) || String(a).localeCompare(String(b)))
  return sorted[0] || ''
}

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

function relPcsFromStepPattern(edo, pattern) {
  const steps = String(pattern)
    .split('-')
    .map((x) => Number.parseInt(x, 10))
    .filter((x) => Number.isFinite(x) && x > 0)
  if (steps.length === 0) return []
  const rel = [0]
  let acc = 0
  for (const s of steps) {
    acc += s
    // Scala patterns should stay within the octave; guard anyway.
    rel.push(normPc(edo, acc))
  }
  return uniqSorted(rel)
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
  return `${name}~${v}c`
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

  const approxByEdo = new Map() // edo -> Map(pattern -> string[])
  const embed12ByEdo = new Map() // edo -> Map(pattern -> string[])

  const patterns12 = byEdoExact.get(12) || new Map()

  function ensureForEdo(targetEdo) {
    if (!approxByEdo.has(targetEdo)) {
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

    if (!embed12ByEdo.has(targetEdo)) {
      const m = new Map()
      // For EDOs divisible by 12, embed 12-EDO step patterns as exact equivalents.
      // For EDO=12 itself this is redundant (it would produce duplicate -EDO12 names),
      // so skip it.
      if (targetEdo !== 12 && targetEdo % 12 === 0 && patterns12.size) {
        const f = targetEdo / 12
        for (const [pat12, names12] of patterns12.entries()) {
          const steps12 = String(pat12)
            .split('-')
            .map((x) => Number.parseInt(x, 10))
            .filter((x) => Number.isFinite(x) && x > 0)
          if (steps12.length === 0) continue
          const patN = steps12.map((s) => String(s * f)).join('-')
          const names = Array.isArray(names12) ? names12 : []
          const outNames = names
            .filter(Boolean)
            .map((n) => `${String(n)}-EDO12`)

          if (!m.has(patN)) m.set(patN, [])
          const arr = m.get(patN)
          for (const nm of outNames) {
            if (!arr.includes(nm)) arr.push(nm)
          }
        }
      }
      embed12ByEdo.set(targetEdo, m)
    }
  }

  // Precompute for all EDOs that exist in the file.
  for (const e of byEdoExact.keys()) ensureForEdo(e)

  return { byEdoExact, embed12ByEdo, approxByEdo, ensureForEdo }
}

export function findChordNames(db, edo, pitchClasses) {
  if (!db || !db.byEdoExact) return []
  db.ensureForEdo?.(edo)
  const patternsExact = db.byEdoExact.get(edo) || new Map()
  const patternsEmbed12 = edo === 12 ? new Map() : db.embed12ByEdo?.get(edo) || new Map()
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
    const namesE = pattern ? patternsEmbed12.get(pattern) || [] : []
    const names1 = pattern ? patternsApprox.get(pattern) || [] : []
    // Keep exact names first; then exact EDO12-embedded names; then approximations.
    const names = [...names0]
    for (const n of namesE) {
      if (!names.includes(n)) names.push(n)
    }
    for (const n of names1) {
      if (!names.includes(n)) names.push(n)
    }
    out.push({ rootPc, rel, pattern, names })
  }
  return out
}

export function buildChordCatalogue(db, edo, opts = {}) {
  if (!db || !db.byEdoExact) return { edo, results: [] }
  const e = Number.parseInt(String(edo ?? ''), 10)
  if (!Number.isInteger(e) || e < 1 || e > 999) return { edo: e, results: [] }

  db.ensureForEdo?.(e)
  const patternsExact = db.byEdoExact.get(e) || new Map()
  const patternsEmbed12 = e === 12 ? new Map() : db.embed12ByEdo?.get(e) || new Map()
  const patternsApprox = db.approxByEdo?.get(e) || new Map()

  const maxTones = Number.parseInt(String(opts.maxTones ?? 4), 10)
  const minTones = Number.parseInt(String(opts.minTones ?? 3), 10)
  const limit = Number.parseInt(String(opts.limit ?? 250), 10)

  /** @type {Map<string, { pattern:string, pcsRoot:number[], names:string[] }>} */
  const byPcsKey = new Map()

  const allPatterns = new Set()
  for (const k of patternsExact.keys()) allPatterns.add(k)
  for (const k of patternsEmbed12.keys()) allPatterns.add(k)
  for (const k of patternsApprox.keys()) allPatterns.add(k)

  for (const pattern of allPatterns.values()) {
    const pcsRoot = relPcsFromStepPattern(e, pattern)
    if (!pcsRoot.length) continue
    if (pcsRoot[0] !== 0) continue
    if (pcsRoot.length < minTones || pcsRoot.length > maxTones) continue

    const names0 = patternsExact.get(pattern) || []
    const namesE = patternsEmbed12.get(pattern) || []
    const names1 = patternsApprox.get(pattern) || []
    const names = [...names0]
    for (const n of namesE) if (!names.includes(n)) names.push(n)
    for (const n of names1) if (!names.includes(n)) names.push(n)

    const key = pcsRoot.join(',')
    const prev = byPcsKey.get(key)
    if (!prev) {
      byPcsKey.set(key, { pattern: String(pattern), pcsRoot, names })
    } else {
      // Merge names; keep the earlier pattern string.
      for (const n of names) if (!prev.names.includes(n)) prev.names.push(n)
    }
  }

  const out = []
  for (const v of byPcsKey.values()) {
    let allNames = [...v.names].filter(Boolean)
    if (e === 12) {
      // In 12-EDO, hide redundant -EDO12 suffix variants.
      allNames = allNames.filter((n) => !String(n).toLowerCase().endsWith('-edo12'))
      // Also dedupe: keep only one of (X) vs (X-EDO12) if both slipped in.
      const set = new Set(allNames.map((n) => String(n)))
      allNames = allNames.filter((n) => {
        const s = String(n)
        if (set.has(`${s}-EDO12`)) return true
        return true
      })
    }

    allNames = allNames.sort((a, b) => nameScore(a) - nameScore(b) || String(a).localeCompare(String(b)))
    const bn = bestName(allNames)
    out.push({ pcsRoot: v.pcsRoot, pattern: v.pattern, bestName: bn, allNames })
  }

  out.sort((a, b) => {
    const sa = nameScore(a.bestName)
    const sb = nameScore(b.bestName)
    if (sa !== sb) return sa - sb
    if (a.pcsRoot.length !== b.pcsRoot.length) return a.pcsRoot.length - b.pcsRoot.length
    return a.bestName.localeCompare(b.bestName)
  })

  return { edo: e, results: out.slice(0, Number.isFinite(limit) ? Math.max(1, limit) : 250) }
}
