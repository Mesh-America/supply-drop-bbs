// Full-screen "restore in progress" notice that blocks the whole admin UI until
// the restarted service is back, then reloads the page.
//
// Built with plain DOM calls instead of a Vue component on purpose: it has to
// outlive the page that opened it. Browser Back/Forward or a hash edit
// unmounts the Backups page, and a Vue-rendered overlay would vanish then
// while the restart is still pending. Once shown it stays until the page
// reloads, except in the one case `giveUpAfterSeconds` covers.

import { fetchBootId, watchForRestart } from './restartWatch'

const OVERLAY_ID = 'restore-wait-overlay'
/** After this long without seeing the restart, offer a way out. */
const SLOW_AFTER_SECONDS = 120

export interface RestoreWaitOptions {
  /** The server's own message, shown large in red. */
  message: string
  /** True when nothing will restart the service by itself. */
  restartByHand: boolean
  /** Boot id read before the restore was triggered (see fetchBootId). */
  baselineBootId: string | null
  /**
   * For a request whose outcome is unknown (it failed before an answer). A
   * confirmed restore makes the service exit within a moment, so if the same
   * process is still answering after this many seconds no restart is coming
   * and the restore was not applied: dismiss the notice and call `onGiveUp`.
   * Not used when the server itself confirmed the restore.
   */
  giveUpAfterSeconds?: number
  onGiveUp?: () => void
}

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  style: string,
  text?: string
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag)
  node.style.cssText = style
  if (text !== undefined) node.textContent = text
  return node
}

export { fetchBootId }

export function showRestoreWait(opts: RestoreWaitOptions): void {
  if (document.getElementById(OVERLAY_ID)) return

  // The box centres itself with auto margins rather than the overlay using
  // align-items:center, so content taller than the viewport scrolls from the
  // top instead of being clipped at both ends.
  const overlay = el(
    'div',
    'position:fixed;inset:0;z-index:2147483647;display:flex;overflow-y:auto;' +
      'padding:1.5rem;background:rgba(0,0,0,0.95);text-align:center;cursor:wait;' +
      'outline:none;color:#f5f5f5;font-family:system-ui,sans-serif;'
  )
  overlay.id = OVERLAY_ID
  overlay.setAttribute('role', 'alertdialog')
  overlay.setAttribute('aria-modal', 'true')
  overlay.setAttribute('aria-labelledby', `${OVERLAY_ID}-title`)
  overlay.setAttribute('aria-describedby', `${OVERLAY_ID}-text`)
  overlay.tabIndex = -1

  const box = el(
    'div',
    'display:flex;flex-direction:column;align-items:center;gap:1rem;' +
      'max-width:46rem;margin:auto;'
  )

  const spinner = el(
    'div',
    'width:3rem;height:3rem;border:0.3rem solid rgba(255,77,77,0.25);' +
      'border-top-color:#ff4d4d;border-radius:50%;flex:none;'
  )
  spinner.setAttribute('aria-hidden', 'true')
  const reduceMotion = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
  if (!reduceMotion && typeof spinner.animate === 'function') {
    spinner.animate([{ transform: 'rotate(0deg)' }, { transform: 'rotate(360deg)' }], {
      duration: 1000,
      iterations: Infinity,
    })
  }

  const title = el(
    'h2',
    'margin:0;font-size:clamp(1.6rem,7vw,2.4rem);line-height:1.2;font-weight:700;' +
      'color:#ff4d4d;overflow-wrap:anywhere;',
    opts.message
  )
  title.id = `${OVERLAY_ID}-title`

  const text = el(
    'p',
    'margin:0;font-size:1.15rem;line-height:1.5;color:#f5f5f5;',
    opts.restartByHand
      ? 'Restart the BBS now, the way you normally start it, to apply the restore. ' +
          'This page reloads by itself when the BBS is back.'
      : 'Do not close or use this page. It reloads by itself when the service is back.'
  )
  text.id = `${OVERLAY_ID}-text`

  // Not a live region: it changes every poll, which would interrupt a screen
  // reader each time.
  const timer = el(
    'p',
    'margin:0;font-size:1rem;color:#b8b8b8;font-variant-numeric:tabular-nums;',
    'waiting 0s'
  )

  // Revealed after SLOW_AFTER_SECONDS. A status region so it is announced.
  const slow = el('div', 'display:none;flex-direction:column;align-items:center;gap:0.75rem;')
  slow.setAttribute('role', 'status')
  slow.append(
    el(
      'p',
      'margin:0;font-size:1.05rem;line-height:1.5;color:#f5f5f5;',
      'This is taking longer than expected. Check that the BBS is running and ' +
        'look at its logs, then reload this page.'
    )
  )
  const reload = el(
    'button',
    'font:inherit;font-size:1rem;padding:0.5rem 1.2rem;border-radius:4px;' +
      'border:1px solid #ff4d4d;background:transparent;color:#ff4d4d;cursor:pointer;',
    'Reload this page'
  )
  reload.type = 'button'
  reload.addEventListener('click', () => window.location.reload())
  slow.append(reload)

  box.append(spinner, title, text, timer, slow)
  overlay.append(box)
  document.body.append(overlay)

  // Everything behind the notice, keyboard and assistive tech included, stops
  // responding. The overlay is outside #app so it stays live.
  const app = document.getElementById('app')
  app?.setAttribute('inert', '')
  overlay.focus()

  let slowShown = false
  const stop = watchForRestart({
    baselineBootId: opts.baselineBootId,
    onTick: (seconds, sameProcessAnswering) => {
      timer.textContent = `waiting ${seconds}s`
      if (
        opts.giveUpAfterSeconds !== undefined &&
        seconds >= opts.giveUpAfterSeconds &&
        sameProcessAnswering
      ) {
        stop()
        overlay.remove()
        app?.removeAttribute('inert')
        opts.onGiveUp?.()
        return
      }
      if (seconds >= SLOW_AFTER_SECONDS && !slowShown) {
        slowShown = true
        slow.style.display = 'flex'
        reload.focus()
      }
    },
    onRestarted: () => window.location.reload(),
  })
}
