import { useQuery } from '@tanstack/react-query'
import { api, type ListParams } from '../api/client'

export function useTorrents(params: ListParams) {
  return useQuery({
    queryKey: ['torrents', params],
    queryFn: () => api.torrents.list(params),
    placeholderData: (prev) => prev,
  })
}

export function useHealth() {
  return useQuery({
    queryKey: ['health'],
    queryFn: api.health,
    refetchInterval: 10000,
  })
}
