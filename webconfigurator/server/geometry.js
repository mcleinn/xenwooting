import fs from 'node:fs/promises'

// 4x14 playable grid (56 cells). Not every cell maps to a physical key on ANSI 60%.
//
// xenWooting.json contains key *geometry* and current keymap. The keymap values are user-configurable,
// so we do NOT use them to identify physical keys. Instead we map keys by position (row y, then x).

const HID = {
  Escape: 0x29,
  Digit1: 0x1e,
  Digit2: 0x1f,
  Digit3: 0x20,
  Digit4: 0x21,
  Digit5: 0x22,
  Digit6: 0x23,
  Digit7: 0x24,
  Digit8: 0x25,
  Digit9: 0x26,
  Digit0: 0x27,
  Minus: 0x2d,
  Equal: 0x2e,
  Backspace: 0x2a,

  Tab: 0x2b,
  KeyQ: 0x14,
  KeyW: 0x1a,
  KeyE: 0x08,
  KeyR: 0x15,
  KeyT: 0x17,
  KeyY: 0x1c,
  KeyU: 0x18,
  KeyI: 0x0c,
  KeyO: 0x12,
  KeyP: 0x13,
  BracketLeft: 0x2f,
  BracketRight: 0x30,
  Backslash: 0x31,

  CapsLock: 0x39,
  KeyA: 0x04,
  KeyS: 0x16,
  KeyD: 0x07,
  KeyF: 0x09,
  KeyG: 0x0a,
  KeyH: 0x0b,
  KeyJ: 0x0d,
  KeyK: 0x0e,
  KeyL: 0x0f,
  Semicolon: 0x33,
  Quote: 0x34,
  Enter: 0x28,

  ShiftLeft: 0xe1,
  KeyZ: 0x1d,
  KeyX: 0x1b,
  KeyC: 0x06,
  KeyV: 0x19,
  KeyB: 0x05,
  KeyN: 0x11,
  KeyM: 0x10,
  Comma: 0x36,
  Period: 0x37,
  Slash: 0x38,
  ShiftRight: 0xe5,
}

export async function loadPlayableGeometry(xenWootingJsonPath) {
  const raw = await fs.readFile(xenWootingJsonPath, 'utf8')
  const keys = JSON.parse(raw)

  /** @type {Map<number, Array<any>>} */
  const byY = new Map()
  for (const k of keys) {
    const y = k?.layout?.y
    if (typeof y !== 'number') continue
    if (y < 0 || y > 3) continue // playable rows only
    const arr = byY.get(y) || []
    arr.push(k)
    byY.set(y, arr)
  }

  for (const [y, arr] of byY.entries()) {
    arr.sort((a, b) => a.layout.x - b.layout.x)
    byY.set(y, arr)
  }

  const rows = [
    [
      HID.Escape,
      HID.Digit1,
      HID.Digit2,
      HID.Digit3,
      HID.Digit4,
      HID.Digit5,
      HID.Digit6,
      HID.Digit7,
      HID.Digit8,
      HID.Digit9,
      HID.Digit0,
      HID.Minus,
      HID.Equal,
      HID.Backspace,
    ],
    [
      HID.Tab,
      HID.KeyQ,
      HID.KeyW,
      HID.KeyE,
      HID.KeyR,
      HID.KeyT,
      HID.KeyY,
      HID.KeyU,
      HID.KeyI,
      HID.KeyO,
      HID.KeyP,
      HID.BracketLeft,
      HID.BracketRight,
      HID.Backslash,
    ],
    [
      HID.CapsLock,
      HID.KeyA,
      HID.KeyS,
      HID.KeyD,
      HID.KeyF,
      HID.KeyG,
      HID.KeyH,
      HID.KeyJ,
      HID.KeyK,
      HID.KeyL,
      HID.Semicolon,
      HID.Quote,
      HID.Enter,
      null,
    ],
    [
      HID.ShiftLeft,
      HID.KeyZ,
      HID.KeyX,
      HID.KeyC,
      HID.KeyV,
      HID.KeyB,
      HID.KeyN,
      HID.KeyM,
      HID.Comma,
      HID.Period,
      HID.Slash,
      HID.ShiftRight,
      null,
      null,
    ],
  ]

  /** @type {Array<{idx:number,row:number,col:number,hidUsage:number,x:number,y:number,w:number,h:number}>} */
  const out = []

  for (let r = 0; r < rows.length; r++) {
    const rowY = r
    const rowKeys = byY.get(rowY) || []

    // Expected physical key count for ANSI 60% top 4 rows:
    // y=0: 14, y=1: 14, y=2: 13, y=3: 12.
    const maxKeys = r === 0 || r === 1 ? 14 : r === 2 ? 13 : 12

    for (let c = 0; c < maxKeys; c++) {
      const key = rowKeys[c]
      if (!key) continue
      const layout = key.layout
      const idx = r * 14 + c
      const hidUsage = rows[r][c] || 0
      out.push({
        idx,
        row: r,
        col: c,
        hidUsage,
        x: layout.x,
        y: layout.y,
        w: layout.width,
        h: layout.height,
      })
    }
  }

  return out
}
