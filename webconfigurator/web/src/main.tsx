import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'
import BoardsPage from './BoardsPage.tsx'
import LivePage from './LivePage.tsx'
import GuidePage from './GuidePage.tsx'
import DumpsPage from './DumpsPage.tsx'

function pickRoot() {
  const p = window.location.pathname.replace(/\/+$/, '')
  if (p === '/wtn/boards') return <BoardsPage />
  if (p === '/wtn/live') return <LivePage />
  if (p === '/wtn/guide') return <GuidePage />
  if (p === '/wtn/dumps') return <DumpsPage />
  return <App />
}

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {pickRoot()}
  </StrictMode>,
)
