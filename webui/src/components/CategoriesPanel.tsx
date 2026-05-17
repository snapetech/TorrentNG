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
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 16 }}>
        <div>
          <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)' }}>Categories</div>
          <div style={{ fontSize: 12, color: 'var(--faint)', marginTop: 2 }}>
            {categories.length.toLocaleString()} configured
          </div>
        </div>
        {(save.isPending || del.isPending) && (
          <span style={{
            color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
            borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 700,
          }}>Working</span>
        )}
      </div>

      {/* Category list */}
      {isLoading ? (
        <div style={{ display: 'grid', gap: 8, marginBottom: 20, maxWidth: 720 }}>
          {Array.from({ length: 3 }, (_, index) => (
            <div key={index} style={{
              border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)',
              padding: '10px 12px', display: 'grid', gap: 8,
            }}>
              <span className="tng-skeleton" style={{ width: index === 1 ? '36%' : '52%', height: 12 }} />
              <span className="tng-skeleton" style={{ width: index === 2 ? '68%' : '44%', height: 8 }} />
            </div>
          ))}
        </div>
      ) : categories.length === 0 ? (
        <div style={{
          fontSize: 12, color: 'var(--faint)', marginBottom: 16,
          border: '1px dashed var(--border-strong)', borderRadius: 7,
          background: 'color-mix(in srgb, var(--surface) 72%, transparent)', padding: 14,
          maxWidth: 720,
        }}>No categories yet.</div>
      ) : (
        <div style={{ marginBottom: 20 }}>
          {deleteError && <Notice tone="error">{deleteError}</Notice>}
          {categories.map(cat => (
            <div key={cat.name} className="tng-card tng-category-row" data-active={editingName === cat.name ? 'true' : 'false'} data-has-path={cat.save_path ? 'true' : 'false'} style={{
              display: 'grid',
              gridTemplateColumns: 'minmax(0, 1fr) auto auto',
              alignItems: 'center',
              gap: 12,
              padding: '10px 12px',
              background: editingName === cat.name ? 'var(--accent-soft)' : 'var(--surface)',
              border: '1px solid ' + (editingName === cat.name ? 'var(--accent)' : 'var(--border)'),
              borderRadius: 6,
              marginBottom: 6,
            }}>
              <div style={{ minWidth: 0 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 7 }}>
                  <span style={{
                    width: 7, height: 7, borderRadius: '50%', background: cat.save_path ? 'var(--success)' : 'var(--warning)',
                    flexShrink: 0,
                  }} />
                  <span style={{ fontSize: 13, color: 'var(--text)', fontWeight: 700, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{cat.name}</span>
                </div>
                <div style={{ fontSize: 11, color: 'var(--faint)', fontFamily: 'monospace', marginTop: 4, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
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
      <form className="tng-form-card tng-category-form" onSubmit={submit} style={{
        display: 'flex', flexDirection: 'column', gap: 10, maxWidth: 520,
        border: '1px solid var(--border)', borderRadius: 8, background: 'var(--surface)', padding: 12,
        boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.03)',
      }}>
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
        {save.isError && <Notice tone="error">Failed to save.</Notice>}
      </form>
    </div>
  )
}

function Notice({ tone, children }: { tone: 'error' | 'ok'; children: React.ReactNode }) {
  return (
    <div style={{
      fontSize: 12,
      color: tone === 'error' ? 'var(--danger)' : 'var(--success)',
      background: tone === 'error' ? 'color-mix(in srgb, var(--danger) 9%, var(--surface))' : 'color-mix(in srgb, var(--success) 8%, var(--surface))',
      border: '1px solid ' + (tone === 'error' ? 'color-mix(in srgb, var(--danger) 45%, var(--border))' : 'color-mix(in srgb, var(--success) 40%, var(--border))'),
      borderRadius: 6,
      padding: '8px 9px',
      marginBottom: 8,
      overflowWrap: 'anywhere',
    }}>{children}</div>
  )
}
