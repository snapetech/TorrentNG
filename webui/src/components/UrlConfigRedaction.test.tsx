import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { api, type RssRule, type WorkflowRule } from '../api/client'
import { RssRulesPanel } from './RssRulesPanel'
import { WorkflowsPanel } from './WorkflowsPanel'

vi.mock('../api/client', () => ({
  api: {
    workflows: {
      list: vi.fn(),
      runs: vi.fn(),
      save: vi.fn(),
      delete: vi.fn(),
      run: vi.fn(),
    },
    rssRules: {
      list: vi.fn(),
      save: vi.fn(),
      delete: vi.fn(),
      test: vi.fn(),
      apply: vi.fn(),
    },
  },
}))

const workflowRule: WorkflowRule = {
  id: 'workflow-1',
  name: 'Private webhook',
  enabled: true,
  event: 'completed',
  action: 'webhook',
  category: null,
  target_category: null,
  tracker: null,
  command: null,
  url: 'https://user:auth-secret@hooks.example/api/webhooks/123/opaque-token?signature=query-secret#fragment-secret',
  target_path: null,
}

const rssRule: RssRule = {
  id: 'rss-1',
  name: 'Private feed',
  enabled: true,
  feed_url: 'https://rss-user:rss-auth-secret@feeds.example/private/rss?key=rss-query-secret',
  include: 'release',
  exclude: null,
  category: null,
  save_path: null,
  tags: [],
  start: true,
}

function renderWithQueryClient(element: ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  })
  return render(<QueryClientProvider client={client}>{element}</QueryClientProvider>)
}

describe('configuration URL labels', () => {
  beforeEach(() => {
    vi.mocked(api.workflows.list).mockResolvedValue([workflowRule])
    vi.mocked(api.workflows.runs).mockResolvedValue([])
    vi.mocked(api.rssRules.list).mockResolvedValue([rssRule])
  })

  afterEach(() => {
    cleanup()
    vi.clearAllMocks()
  })

  it('does not render webhook credentials or path/query secrets in workflow rows', async () => {
    const { container } = renderWithQueryClient(<WorkflowsPanel />)

    expect(await screen.findByText('https://hooks.example/…')).toBeTruthy()
    for (const secret of ['auth-secret', 'opaque-token', 'query-secret', 'fragment-secret']) {
      expect(container.textContent).not.toContain(secret)
    }
  })

  it('does not render credentials or feed tokens in RSS rule rows', async () => {
    const { container } = renderWithQueryClient(<RssRulesPanel />)

    expect(await screen.findByText('https://feeds.example/…')).toBeTruthy()
    for (const secret of ['rss-auth-secret', 'private/rss', 'rss-query-secret']) {
      expect(container.textContent).not.toContain(secret)
    }
  })
})
