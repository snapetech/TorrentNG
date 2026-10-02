import { useEffect, useState, type CSSProperties, type FormEvent } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type AuthSettings } from '../api/client'

export function AuthPanel() {
  const queryClient = useQueryClient()
  const { data, isLoading, error } = useQuery({
    queryKey: ['auth-settings'],
    queryFn: api.auth.getSettings,
  })
  const [username, setUsername] = useState('torrentng')
  const [password, setPassword] = useState('')
  const [notice, setNotice] = useState('')
  const save = useMutation({
    mutationFn: api.auth.updateSettings,
    onSuccess: (settings: AuthSettings) => {
      queryClient.setQueryData(['auth-settings'], settings)
      setUsername(settings.username)
      setPassword('')
      setNotice('Login credentials saved.')
    },
    onError: () => setNotice('Could not save login credentials.'),
  })
  const reset = useMutation({
    mutationFn: api.auth.resetSettings,
    onSuccess: (settings: AuthSettings) => {
      queryClient.setQueryData(['auth-settings'], settings)
      setUsername(settings.username)
      setPassword('')
      setNotice('Credentials restored from config.toml.')
    },
    onError: () => setNotice('Could not restore credentials from config.toml.'),
  })

  useEffect(() => {
    if (data) setUsername(data.username)
  }, [data])

  function submit(event: FormEvent) {
    event.preventDefault()
    setNotice('')
    save.mutate({ username, password })
  }

  return (
    <section aria-labelledby="auth-settings-title" style={{ padding: '16px 24px', maxWidth: 760 }}>
      <h2 id="auth-settings-title" style={{ margin: '0 0 5px', fontSize: 14, color: 'var(--text)' }}>
        Login credentials
      </h2>
      <p style={{ margin: '0 0 14px', color: 'var(--faint)', fontSize: 12, lineHeight: 1.55 }}>
        Choose a WebUI username and password. Public binds require a password of at least 16 characters. API tokens stay enabled for automation and can also be entered in either login field.
      </p>

      {isLoading && <div role="status">Loading authentication settings…</div>}
      {error && <div role="alert" style={{ color: 'var(--danger)', fontSize: 12 }}>Authentication settings are unavailable.</div>}
      {data && (
        <>
          <div style={{ marginBottom: 14, fontSize: 12, color: 'var(--muted)', lineHeight: 1.55 }}>
            <div>Current username: <strong>{data.username}</strong></div>
            <div>API-token login: <strong>{data.api_token_login_enabled ? 'enabled' : 'not configured'}</strong></div>
          </div>

          <form onSubmit={submit} style={{ display: 'grid', gap: 10, maxWidth: 440 }}>
            <label style={labelStyle}>
              Username
              <input
                value={username}
                onChange={event => setUsername(event.target.value)}
                autoComplete="username"
                required
                maxLength={256}
                style={inputStyle}
              />
            </label>
            <label style={labelStyle}>
              New password
              <input
                type="password"
                value={password}
                onChange={event => setPassword(event.target.value)}
                autoComplete="new-password"
                required
                minLength={8}
                maxLength={1024}
                style={inputStyle}
              />
            </label>
            <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8 }}>
              <button type="submit" disabled={save.isPending || reset.isPending} style={buttonStyle}>
                {save.isPending ? 'Saving…' : 'Save credentials'}
              </button>
              <button type="button" onClick={() => reset.mutate()} disabled={save.isPending || reset.isPending} style={secondaryButtonStyle}>
                {reset.isPending ? 'Restoring…' : 'Restore config.toml credentials'}
              </button>
            </div>
          </form>
          {notice && <div role="status" aria-live="polite" style={{ marginTop: 10, color: 'var(--muted)', fontSize: 12 }}>{notice}</div>}

          <div style={{ marginTop: 16, padding: 11, border: '1px solid var(--border)', borderRadius: 6, color: 'var(--faint)', fontSize: 11, lineHeight: 1.55 }}>
            API tokens are configured in <code>[auth].api_tokens</code> or <code>[auth].api_tokens_file</code>. On Unraid, find the token in the container’s <strong>API Token</strong> setting; the existing-client profile also accepts <code>TNG_API_TOKENS</code> or <code>RTNG_API_TOKENS</code>. Runtime credential changes are stored in the service state directory; reset here to use values from config.toml again.
          </div>
        </>
      )}
    </section>
  )
}

const labelStyle: CSSProperties = {
  display: 'grid',
  gap: 5,
  color: 'var(--muted)',
  fontSize: 12,
}

const inputStyle: CSSProperties = {
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '8px 10px',
  fontSize: 13,
}

const buttonStyle: CSSProperties = {
  background: 'var(--accent-soft)',
  border: '1px solid var(--accent)',
  borderRadius: 5,
  color: 'var(--accent-text)',
  padding: '7px 10px',
  fontSize: 12,
  cursor: 'pointer',
}

const secondaryButtonStyle: CSSProperties = {
  ...buttonStyle,
  background: 'var(--surface)',
  borderColor: 'var(--border-strong)',
  color: 'var(--muted)',
}
