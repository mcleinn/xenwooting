export type LayoutInfo = {
  id: string
  name: string
  wtnPath: string
}

export type Cell = {
  note: number
  chan: number
  col: string // RRGGBB (no #)
  // Optional UI-only flag. If false, treat as "unset" for display/cache.
  set?: boolean
}

export type Boards = {
  Board0: Cell[]
  Board1: Cell[]
}

export type LayoutDetail = {
  id: string
  name: string
  boards: Boards
  edoDivisions: number
  pitchOffset: number
}

export type GeometryKey = {
  idx: number
  row: number
  col: number
  hidUsage: number
  x: number
  y: number
  w: number
  h: number
}

export type Geometry = {
  source: string
  width: number
  height: number
  keys: GeometryKey[]
}
