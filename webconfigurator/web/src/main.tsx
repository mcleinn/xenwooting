import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'
import BoardsPage from './BoardsPage.tsx'

function pickRoot() {
  const p = window.location.pathname.replace(/\/+$/, '')
  return p === '/wtn/boards' ? <BoardsPage /> : <App />
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {pickRoot()}
  </StrictMode>,
)
