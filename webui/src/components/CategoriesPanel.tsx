import { useState } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'

interface Category { name: string; save_path: string }

async function fetchCategories(): Promise<Category[]> {
  const res = await fetch('/api/v1/categories')
  if (!res.ok) throw new Error('fetch categories')
  return res.json()
}

async function upsertCategory(cat: Category): Promise<void> {
  const res = await fetch('/api/v1/categories', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', 'X-RTNG-CSRF': '1' },
    body: JSON.stringify(cat),
  })
  if (!res.ok) throw new Error('save category')
}

async function deleteCategory(name: string): Promise<void> {
  const res = await fetch(`/api/v1/categories/${encodeURIComponent(name)}`, {
    method: 'DELETE',
    headers: { 'X-RTNG-CSRF': '1' },
  })
  if (!res.ok) throw new Error('delete category')
}

const INPUT: React.CSSProperties = {
  background: '#0f1117',
  border: '1px solid #334155',
  borderRadius: 5,
  color: '#e2e8f0',
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
    queryFn: fetchCategories,
  })

  const [name, setName] = useState('')
  const [savePath, setSavePath] = useState('')
  const [editingName, setEditingName] = useState<string | null>(null)

  const save = useMutation({
    mutationFn: upsertCategory,
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['categories'] })
      setName('')
      setSavePath('')
      setEditingName(null)
    },
  })

  const del = useMutation({
    mutationFn: deleteCategory,
    onSuccess: () => qc.invalidateQueries({ queryKey: ['categories'] }),
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
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 16, color: '#e2e8f0' }}>
        Categories
      </div>

      {/* Category list */}
      {isLoading ? (
        <div style={{ fontSize: 12, color: '#64748b', marginBottom: 16 }}>Loading…</div>
      ) : categories.length === 0 ? (
        <div style={{ fontSize: 12, color: '#475569', marginBottom: 16 }}>No categories yet.</div>
      ) : (
        <div style={{ marginBottom: 20 }}>
          {categories.map(cat => (
            <div key={cat.name} style={{
              display: 'flex',
              alignItems: 'center',
              gap: 12,
              padding: '8px 12px',
              background: '#1e2433',
              borderRadius: 6,
              marginBottom: 6,
            }}>
              <div style={{ flex: 1 }}>
                <div style={{ fontSize: 13, color: '#e2e8f0', fontWeight: 500 }}>{cat.name}</div>
                <div style={{ fontSize: 11, color: '#475569', fontFamily: 'monospace', marginTop: 2 }}>
                  {cat.save_path || '(no save path)'}
                </div>
              </div>
              <button
                onClick={() => startEdit(cat)}
                style={{ background: 'none', border: '1px solid #334155', borderRadius: 4, color: '#94a3b8', padding: '2px 8px', fontSize: 11, cursor: 'pointer' }}
              >Edit</button>
              <button
                onClick={() => del.mutate(cat.name)}
                style={{ background: 'none', border: '1px solid #7f1d1d', borderRadius: 4, color: '#ef4444', padding: '2px 8px', fontSize: 11, cursor: 'pointer' }}
              >Delete</button>
            </div>
          ))}
        </div>
      )}

      {/* Add / edit form */}
      <form onSubmit={submit} style={{ display: 'flex', flexDirection: 'column', gap: 10, maxWidth: 420 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: '#64748b', letterSpacing: '0.05em', textTransform: 'uppercase' }}>
          {editingName ? `Edit "${editingName}"` : 'Add category'}
        </div>
        <div>
          <label style={{ fontSize: 11, color: '#64748b', display: 'block', marginBottom: 4 }}>Name</label>
          <input
            value={name}
            onChange={e => setName(e.target.value)}
            placeholder="e.g. Movies"
            disabled={!!editingName}
            style={{ ...INPUT, opacity: editingName ? 0.6 : 1 }}
          />
        </div>
        <div>
          <label style={{ fontSize: 11, color: '#64748b', display: 'block', marginBottom: 4 }}>Save path</label>
          <input
            value={savePath}
            onChange={e => setSavePath(e.target.value)}
            placeholder="/data/downloads/movies"
            style={INPUT}
          />
        </div>
        <div style={{ display: 'flex', gap: 8 }}>
          <button type="submit" disabled={save.isPending} style={{
            background: '#1e3a5f', border: '1px solid #3b82f6', borderRadius: 5,
            color: '#93c5fd', padding: '5px 16px', fontSize: 13, cursor: 'pointer',
          }}>
            {save.isPending ? 'Saving…' : editingName ? 'Save changes' : 'Add'}
          </button>
          {editingName && (
            <button type="button" onClick={cancelEdit} style={{
              background: 'none', border: '1px solid #334155', borderRadius: 5,
              color: '#64748b', padding: '5px 12px', fontSize: 13, cursor: 'pointer',
            }}>Cancel</button>
          )}
        </div>
        {save.isError && <div style={{ fontSize: 12, color: '#ef4444' }}>Failed to save.</div>}
      </form>
    </div>
  )
}
