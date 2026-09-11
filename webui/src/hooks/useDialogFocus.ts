import { useEffect, useRef } from 'react'

const FOCUSABLE = [
  'button:not([disabled])',
  '[href]',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])',
].join(',')

function visibleFocusable(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE))
    .filter(element => element.getClientRects().length > 0)
}

/** Keep keyboard focus inside a transient surface and return it to the opener. */
export function useDialogFocus<T extends HTMLElement = HTMLDivElement>(onClose: () => void) {
  const dialogRef = useRef<T>(null)
  const closeRef = useRef(onClose)
  closeRef.current = onClose

  useEffect(() => {
    const dialog = dialogRef.current
    if (!dialog) return
    const dialogElement: HTMLElement = dialog

    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null
    const focusInitial = () => {
      const initial = dialog.querySelector<HTMLElement>('[data-dialog-initial-focus], [autofocus]')
      ;(initial ?? visibleFocusable(dialog)[0] ?? dialog).focus()
    }
    const animationFrame = window.requestAnimationFrame(focusInitial)

    function onKeyDown(event: KeyboardEvent) {
      if (!dialogElement.contains(event.target as Node)) return
      if (event.key === 'Escape') {
        event.preventDefault()
        closeRef.current()
        return
      }
      if (event.key !== 'Tab') return

      const focusable = visibleFocusable(dialogElement)
      if (focusable.length === 0) {
        event.preventDefault()
        dialogElement.focus()
        return
      }

      const currentIndex = focusable.indexOf(document.activeElement as HTMLElement)
      const first = currentIndex <= 0
      const last = currentIndex === focusable.length - 1 || currentIndex === -1
      if ((event.shiftKey && first) || (!event.shiftKey && last)) {
        event.preventDefault()
        const target = event.shiftKey ? focusable[focusable.length - 1] : focusable[0]
        target.focus()
      }
    }

    dialogElement.addEventListener('keydown', onKeyDown)
    return () => {
      window.cancelAnimationFrame(animationFrame)
      dialogElement.removeEventListener('keydown', onKeyDown)
      if (previous?.isConnected) previous.focus()
    }
  }, [])

  return dialogRef
}
