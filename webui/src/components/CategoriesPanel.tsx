import { useState } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import type { Category } from '../api/client'

const INPUT: React.CSSProperties = {
  background: 'var(--bg)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '5px 10px',
  fontSize: 13,
  outline: 'none',
  width: '100%',
  boxSizing: 'border-box',
}

export function CategoriesPanel() {
  const qc = useQueryClient()
  const { data: categories = [], isLoading } = useQuery({
    queryKey: ['categories'],
    queryFn: api.categories.list,
  })

  const [name, setName] = useState('')
  const [savePath, setSavePath] = useState('')
  const [editingName, setEditingName] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  const save = useMutation({
    mutationFn: (cat: Category) => api.categories.create(cat.name, cat.save_path),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['categories'] })
      setName('')
      setSavePath('')
      setEditingName(null)
    },
  })

  const del = useMutation({
    mutationFn: (name: string) => api.categories.delete(name),
    onMutate: () => setDeleteError(null),
    onSuccess: () => qc.invalidateQueries({ queryKey: ['categories'] }),
    onError: err => setDeleteError(err instanceof Error ? err.message : 'Failed to delete category.'),
  })

  function startEdit(cat: Category) {
    setEditingName(cat.name)
    setName(cat.name)
    setSavePath(cat.save_path)
  }

  function cancelEdit() {
    setEditingName(null)
    setName('')
    setSavePath('')
  }

  function submit(e: React.FormEvent) {
    e.preventDefault()
    if (!name.trim()) return
    save.mutate({ name: name.trim(), save_path: savePath.trim() })
  }

  return (
    <div style={{ padding: '20px 24px' }}>
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 16, color: 'var(--text)' }}>
        Categories
      </div>

      {/* Category list */}
      {isLoading ? (
        <div style={{ fontSize: 12, color: 'var(--faint)', marginBottom: 16 }}>Loading…</div>
      ) : categories.length === 0 ? (
        <div style={{ fontSize: 12, color: 'var(--faint)', marginBottom: 16 }}>No categories yet.</div>
      ) : (
        <div style={{ marginBottom: 20 }}>
          {deleteError && <div style={{ fontSize: 12, color: 'var(--danger)', marginBottom: 8 }}>{deleteError}</div>}
          {categories.map(cat => (
            <div key={cat.name} style={{
              display: 'flex',
              alignItems: 'center',
              gap: 12,
              padding: '8px 12px',
              background: 'var(--surface-2)',
              borderRadius: 6,
              marginBottom: 6,
            }}>
              <div style={{ flex: 1 }}>
                <div style={{ fontSize: 13, color: 'var(--text)', fontWeight: 500 }}>{cat.name}</div>
                <div style={{ fontSize: 11, color: 'var(--faint)', fontFamily: 'monospace', marginTop: 2 }}>
                  {cat.save_path || '(no save path)'}
                </div>
              </div>
              <button
                onClick={() => startEdit(cat)}
                disabled={save.isPending || del.isPending}
                style={{ background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4, color: 'var(--muted)', padding: '2px 8px', fontSize: 11, cursor: 'pointer' }}
              >Edit</button>
              <button
                onClick={() => del.mutate(cat.name)}
                disabled={save.isPending || del.isPending}
                style={{
                  background: 'none', border: '1px solid #7f1d1d', borderRadius: 4, color: '#ef4444',
                  padding: '2px 8px', fontSize: 11, cursor: save.isPending || del.isPending ? 'not-allowed' : 'pointer',
                  opacity: save.isPending || del.isPending ? 0.5 : 1,
                }}
              >{del.isPending && del.variables === cat.name ? 'Deleting…' : 'Delete'}</button>
            </div>
          ))}
        </div>
      )}

      {/* Add / edit form */}
      <form onSubmit={submit} style={{ display: 'flex', flexDirection: 'column', gap: 10, maxWidth: 420 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: 'var(--faint)', letterSpacing: '0.05em', textTransform: 'uppercase' }}>
          {editingName ? `Edit "${editingName}"` : 'Add category'}
        </div>
        <div>
          <label style={{ fontSize: 11, color: 'var(--faint)', display: 'block', marginBottom: 4 }}>Name</label>
          <input
            value={name}
            onChange={e => setName(e.target.value)}
            placeholder="e.g. Movies"
            disabled={!!editingName}
            style={{ ...INPUT, opacity: editingName ? 0.6 : 1 }}
          />
        </div>
        <div>
          <label style={{ fontSize: 11, color: 'var(--faint)', display: 'block', marginBottom: 4 }}>Save path</label>
          <input
            value={savePath}
            onChange={e => setSavePath(e.target.value)}
            placeholder="/data/downloads/movies"
            style={INPUT}
          />
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <button type="submit" disabled={save.isPending || !name.trim()} style={{
            background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
            color: 'var(--accent-text)', padding: '5px 16px', fontSize: 13,
            cursor: save.isPending || !name.trim() ? 'not-allowed' : 'pointer',
            opacity: save.isPending || !name.trim() ? 0.55 : 1,
          }}>
            {save.isPending ? 'Saving…' : editingName ? 'Save changes' : 'Add'}
          </button>
          {editingName && (
            <button type="button" onClick={cancelEdit} style={{
              background: 'none', border: '1px solid var(--border-strong)', borderRadius: 5,
              color: 'var(--faint)', padding: '5px 12px', fontSize: 13, cursor: 'pointer',
            }}>Cancel</button>
          )}
        </div>
        {save.isError && <div style={{ fontSize: 12, color: '#ef4444' }}>Failed to save.</div>}
      </form>
    </div>
  )
}
