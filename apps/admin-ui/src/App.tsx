import {useCallback, useEffect, useRef, useState} from 'react';
import {adminApi} from './api';
import AdminWorkspace from './Workspace';

type AuthState = 'checking' | 'authenticated' | 'unauthenticated';

/** Drops the unpublished announcement and configuration drafts this tab keeps. */
function discardDrafts() {
  try {
    for (const key of Object.keys(sessionStorage)) {
      if (key.startsWith('admin-commercial-draft:') || key === 'admin-announcement-draft:v1') sessionStorage.removeItem(key);
    }
  } catch {/* nothing kept */}
}

export default function App() {
  const [recheckError, setRecheckError] = useState('');
  const [rechecking, setRechecking] = useState(false);
  const recheckPending = useRef(false);
  const [authState, setAuthState] = useState<AuthState>('checking');
  const [workspaceVersion, setWorkspaceVersion] = useState(0);
  const [username, setUsername] = useState('admin');
  const [password, setPassword] = useState('');
  // Uncontrolled: React copies a controlled input's value into its DOM attribute, where
  // markup snapshots and attribute selectors can read it. The DOM property is enough.
  const passwordInput = useRef<HTMLInputElement>(null);
  useEffect(() => {if (!password && passwordInput.current) passwordInput.current.value = '';}, [password]);
  const [totpCode, setTotpCode] = useState('');
  const [totpRequired, setTotpRequired] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  const [expiring, setExpiring] = useState(false);
  const attempt = useRef(0);
  const pending = useRef(false);

  useEffect(() => {
    let current = true;
    const version = ++attempt.current;
    adminApi.onUnauthorized = reason => {
      ++attempt.current;
      setPassword(''); setTotpCode(''); setAuthState('unauthenticated'); setExpiring(false); setRecheckError('');
      setError(reason === 'expired' ? '会话已到期，请重新登录（草稿已保留）' : '');
    };
    adminApi.onExpiring = () => {if (current) setExpiring(true);};
    adminApi.onSessionChanged = () => {
      // The session was replaced underneath this tab, without a login here: nothing the
      // previous session was composing carries over. Only a session that expired and was
      // re-established by logging in again in this tab keeps its drafts.
      discardDrafts();
      if (current) setWorkspaceVersion(value => value + 1);
    };
    adminApi.checkAuth().then(() => {
      if (current && version === attempt.current) setAuthState('authenticated');
    }).catch(() => {
      if (current && version === attempt.current) {adminApi.clearSession(); setAuthState('unauthenticated');}
    }).finally(() => {if (current) setTotpRequired(adminApi.totpRequired);});
    return () => {
      current = false; ++attempt.current;
      adminApi.onUnauthorized = undefined; adminApi.onSessionChanged = undefined; adminApi.onExpiring = undefined;
      adminApi.clearSession();
    };
  }, []);

  // Coming back to the window re-checks the session in the background; the workspace stays
  // usable meanwhile (writes still need a valid session and CSRF token). Only a check that
  // cannot complete covers the workspace; a rejected session goes to the login page.
  const recheck = useCallback(() => {
    if (document.hidden || pending.current || recheckPending.current) return;
    const version = ++attempt.current;
    recheckPending.current = true; setRechecking(true);
    void adminApi.checkAuth()
      .then(() => {if (version === attempt.current) {setRecheckError(''); setAuthState('authenticated');}})
      .catch(() => {if (version === attempt.current) setRecheckError('无法确认登录状态（草稿已保留）');})
      .finally(() => {recheckPending.current = false; setRechecking(false);});
  }, []);

  useEffect(() => {
    if (authState !== 'authenticated') return;
    window.addEventListener('focus', recheck);
    document.addEventListener('visibilitychange', recheck);
    return () => {window.removeEventListener('focus', recheck); document.removeEventListener('visibilitychange', recheck);};
  }, [authState, recheck]);

  async function login() {
    if (pending.current) return;
    pending.current = true; setBusy(true); setError('');
    const version = ++attempt.current;
    try {
      await adminApi.establishSession(username.trim(), password, totpCode || undefined);
      if (version === attempt.current) {setPassword(''); setTotpCode(''); setRecheckError(''); setExpiring(false); setAuthState('authenticated');}
    } catch (cause) {
      if (version === attempt.current) {
        adminApi.clearSession();
        const message = cause instanceof Error ? cause.message : '登录失败，请重试';
        setError(adminApi.totpRequired && message.includes('验证码') ? `${message}。请等下一个验证码再试` : message);
      }
    } finally {pending.current = false; setBusy(false); setPassword(''); setTotpCode(''); setTotpRequired(adminApi.totpRequired);}
  }

  /** Ends the session; `all` revokes every administrator session (the workspace confirms it first). */
  async function logout(all: boolean) {
    if (pending.current) return;
    const version = ++attempt.current;
    pending.current = true; setBusy(true); setPassword(''); setTotpCode(''); setError('');
    // An explicit logout ends the work, so unpublished drafts go with it; only a session
    // that expired keeps them for the next login in this tab.
    discardDrafts();
    // logout clears the API session synchronously, before awaiting the server.
    const revocation = adminApi.logout(all);
    setAuthState('unauthenticated');
    try {await revocation;}
    catch {if (version === attempt.current) setError('已在本机退出，但服务器未确认，建议关闭浏览器');}
    finally {pending.current = false; setBusy(false);}
  }

  /** When the session cannot be confirmed for long (network down): sign in again, drafts kept. */
  const signInAgain = () => {
    ++attempt.current; adminApi.clearSession(); setRecheckError(''); setExpiring(false); setAuthState('unauthenticated');
    setError('请重新登录（草稿已保留）');
  };

  /** Only when the server does not name the operator: sign in again so adjustments have one. */
  const reauthenticate = () => {
    ++attempt.current; adminApi.clearSession(); setAuthState('unauthenticated');
    setError('请重新登录后继续调账');
  };

  if (authState === 'checking') return <main className="auth-page"><p role="status" className="auth-checking">正在检查会话…</p></main>;
  if (authState === 'authenticated') return <>
    <div className="workspace-shell" ref={node => {if (node) node.inert = !!recheckError;}} aria-hidden={recheckError ? true : undefined}>
      <AdminWorkspace key={workspaceVersion} onLogout={logout} operator={adminApi.authenticatedUsername} expiring={expiring} onReauthenticate={reauthenticate}/>
    </div>
    {recheckError && <div className="session-overlay" role="alertdialog" aria-label="无法确认登录状态">
      <section className="session-overlay-card">
        <p>{recheckError}</p>
        <div className="button-row">
          <button type="button" className="btn" disabled={rechecking} onClick={signInAgain}>重新登录</button>
          <button type="button" className="btn btn-primary" disabled={rechecking} onClick={recheck}>{rechecking ? '正在确认…' : '重试'}</button>
        </div>
      </section>
    </div>}
  </>;
  return <main className="auth-page"><section className="auth-card" aria-labelledby="login-title">
    <p className="auth-brand">Superkiro</p>
    <h1 id="login-title">管理员登录</h1>
    <form onSubmit={event => {event.preventDefault(); void login();}} aria-busy={busy}>
      <label className="field"><span className="field-label">用户名</span>
        <input autoComplete="username" required disabled={busy} value={username} onChange={event => setUsername(event.target.value)}/></label>
      <label className="field"><span className="field-label">密码</span>
        <input ref={passwordInput} type="password" autoComplete="current-password" required disabled={busy} onChange={event => setPassword(event.target.value)}/></label>
      {totpRequired && <label className="field"><span className="field-label">动态验证码</span>
        <input aria-label="动态验证码" inputMode="numeric" autoComplete="one-time-code" pattern="[0-9]{6}" maxLength={6} required
          disabled={busy} value={totpCode} onChange={event => setTotpCode(event.target.value)}/></label>}
      {error && <p role="alert" className="auth-error">{error}</p>}
      <button className="btn btn-primary btn-block" type="submit" disabled={busy || !username.trim() || !password}>{busy ? '请稍候…' : '登录'}</button>
    </form>
  </section></main>;
}
