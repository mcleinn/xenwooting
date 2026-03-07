import type { Boards, Geometry, LayoutDetail, LayoutInfo } from './types'

function apiUrl(path: string) {
  // Always resolve from /wtn/ so /wtn, /wtn/live, /wtn/guide behave the same.
  return new URL(path.replace(/^\//, ''), `${window.location.origin}/wtn/`).toString()
}

async function readJsonOrThrow(res: Response) {
  const text = await res.text()
  try {
    return JSON.parse(text)
  } catch {
    const snip = text.slice(0, 120).replace(/\s+/g, ' ')
    if (snip.toLowerCase().startsWith('<!doctype') || snip.toLowerCase().startsWith('<html')) {
      throw new Error(`API returned HTML (server not restarted / dev proxy issue): ${snip}`)
    }
    throw new Error(`API returned non-JSON: ${snip}`)
  }
}

export async function fetchLayouts(): Promise<{ layouts: LayoutInfo[] }> {
  const res = await fetch(apiUrl('api/layouts'))
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function fetchLayout(id: string): Promise<LayoutDetail> {
  const res = await fetch(apiUrl(`api/layout/${encodeURIComponent(id)}`))
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function saveLayout(
  id: string,
  boards: Boards,
): Promise<{ xenwootingReloaded: boolean; xenwootingReloadError: string | null }> {
  const res = await fetch(apiUrl(`api/layout/${encodeURIComponent(id)}`), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ boards }),
  })
  if (!res.ok) throw new Error(await res.text())

  const body = await readJsonOrThrow(res).catch(() => null)
  return {
    xenwootingReloaded: Boolean(body?.xenwootingReloaded),
    xenwootingReloadError: body?.xenwootingReloadError || null,
  }
}

export async function fetchGeometry(): Promise<Geometry> {
  const res = await fetch(apiUrl('api/geometry'))
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function previewEnable(layoutId: string, boards: Boards): Promise<void> {
  const res = await fetch(apiUrl('api/preview/enable'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ layoutId, boards }),
  })
  if (!res.ok) throw new Error(await res.text())
}

export async function previewUpdate(layoutId: string, boards: Boards): Promise<void> {
  const res = await fetch(apiUrl('api/preview/update'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ layoutId, boards }),
  })
  if (!res.ok) throw new Error(await res.text())
}

export async function previewDisable(): Promise<void> {
  const res = await fetch(apiUrl('api/preview/disable'), {
    method: 'POST',
  })
  if (!res.ok) throw new Error(await res.text())
}

export async function highlightKey(layoutId: string, board: 'Board0' | 'Board1', idx: number, down: boolean): Promise<void> {
  const res = await fetch(apiUrl('api/highlight'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ layoutId, board, idx, down }),
  })
  if (!res.ok) throw new Error(await res.text())
}

export async function fetchNoteNames(
  edo: number,
  pitches: number[],
): Promise<{
  edo: number
  results: Record<string, { short: string; unicode: string; alts?: Array<{ short: string; unicode: string }> }>
}> {
  const res = await fetch(apiUrl('api/note-names'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ edo, pitches }),
  })
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function fetchLiveState(): Promise<unknown> {
  const res = await fetch(apiUrl('api/live/state'))
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function fetchChordNames(
  edo: number,
  pitchClasses: number[],
): Promise<{ edo: number; results: Array<{ rootPc: number; rel: number[]; pattern: string; names: string[] }> }> {
  const res = await fetch(apiUrl('api/chord-names'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ edo, pitchClasses }),
  })
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function fetchLayoutIsomorphic(id: string): Promise<{ layoutId: string; ok: boolean; edo: number; dq: number | null; dr: number | null; axis3: number | null; reason: string | null }> {
  const res = await fetch(apiUrl(`api/layout/${encodeURIComponent(id)}/isomorphic`))
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function fetchChordCatalogue(
  edo: number,
  opts?: { limit?: number; minTones?: number; maxTones?: number },
): Promise<{ edo: number; results: Array<{ pcsRoot: number[]; pattern: string; bestName: string; allNames: string[] }> }> {
  const q = new URLSearchParams()
  q.set('edo', String(edo))
  if (opts?.limit) q.set('limit', String(opts.limit))
  if (opts?.minTones) q.set('minTones', String(opts.minTones))
  if (opts?.maxTones) q.set('maxTones', String(opts.maxTones))
  const res = await fetch(apiUrl(`api/chord-catalogue?${q.toString()}`))
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function addLayout(body: {
  name: string
  edoDivisions: number
  pitchOffset?: number
}): Promise<{ id: string; name: string }> {
  const res = await fetch(apiUrl('api/layouts/add'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function updateLayoutSettings(
  id: string,
  body: {
    name: string
    edoDivisions: number
  },
): Promise<{ ok: true }> {
  const res = await fetch(apiUrl(`api/layout/${encodeURIComponent(id)}/settings`), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}

export async function deleteLayout(id: string): Promise<{ ok: true; nextId: string }> {
  const res = await fetch(apiUrl(`api/layout/${encodeURIComponent(id)}`), {
    method: 'DELETE',
  })
  if (!res.ok) throw new Error(await res.text())
  return readJsonOrThrow(res)
}
