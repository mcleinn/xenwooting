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
