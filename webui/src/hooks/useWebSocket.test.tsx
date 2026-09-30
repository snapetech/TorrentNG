import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useWebSocket } from './useWebSocket'

class MockWebSocket {
  static instances: MockWebSocket[] = []
  onmessage: ((event: MessageEvent) => void) | null = null
  onclose: (() => void) | null = null

  constructor(readonly url: string) {
    MockWebSocket.instances.push(this)
  }

  close() {
    this.onclose?.()
  }

  emit(data: string) {
    this.onmessage?.({ data } as MessageEvent)
  }
}

describe('useWebSocket', () => {
  afterEach(() => {
    MockWebSocket.instances = []
    vi.unstubAllGlobals()
  })

  it('invalidates cached projections when the event stream requires a resync', () => {
    vi.stubGlobal('WebSocket', MockWebSocket)
    const queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false } },
    })
    queryClient.setQueryData(['torrents', { filter: 'all' }], ['cached torrent'])
    queryClient.setQueryData(['categories'], ['cached category'])
    const wrapper = ({ children }: { children: React.ReactNode }) => (
      <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>
    )

    const { unmount } = renderHook(() => useWebSocket(undefined, true), { wrapper })
    const socket = MockWebSocket.instances[0]
    expect(socket).toBeDefined()

    act(() => {
      socket.emit(JSON.stringify({
        type: 'resync_required',
        reason: 'event_stream_lagged',
        dropped: 3,
      }))
    })

    for (const query of queryClient.getQueryCache().getAll()) {
      expect(query.state.isInvalidated).toBe(true)
    }
    unmount()
    queryClient.clear()
  })
})
