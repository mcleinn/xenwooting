import type { Boards, Geometry, LayoutDetail, LayoutInfo } from './types'

function apiUrl(path: string) {
  // Served under /wtn/ (Vite base). Keep requests relative.
  return new URL(path.replace(/^\//, ''), window.location.href).toString()
}

export async function fetchLayouts(): Promise<{ layouts: LayoutInfo[] }> {
  const res = await fetch(apiUrl('api/layouts'))
  if (!res.ok) throw new Error(await res.text())
  return res.json()
}

export async function fetchLayout(id: string): Promise<LayoutDetail> {
  const res = await fetch(apiUrl(`api/layout/${encodeURIComponent(id)}`))
  if (!res.ok) throw new Error(await res.text())
  return res.json()
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

  const body = await res.json().catch(() => null)
  return {
    xenwootingReloaded: Boolean(body?.xenwootingReloaded),
    xenwootingReloadError: body?.xenwootingReloadError || null,
  }
}

export async function fetchGeometry(): Promise<Geometry> {
  const res = await fetch(apiUrl('api/geometry'))
  if (!res.ok) throw new Error(await res.text())
  return res.json()
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
): Promise<{ edo: number; results: Record<string, { short: string; unicode: string }> }> {
  const res = await fetch(apiUrl('api/note-names'), {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ edo, pitches }),
  })
  if (!res.ok) throw new Error(await res.text())
  return res.json()
}
