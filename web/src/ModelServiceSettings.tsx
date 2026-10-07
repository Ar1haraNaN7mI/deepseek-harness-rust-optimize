import { useEffect, useState, type FormEvent } from "react";
import { errorText, isAbort, rpc } from "./api";

type Connection = {
  backend: string; base_url: string | null; model: string; temperature: number;
  max_tokens: number; thinking: boolean; credential_configured: boolean;
  saved_credential: boolean; ready: boolean; fallback_count: number;
  send_local_api_key: boolean;
  backends: { id: string; label: string; base_url: string; requires_key: boolean }[];
};
type Draft = Pick<Connection, "backend" | "model" | "temperature" | "max_tokens" | "thinking" | "send_local_api_key"> & { base_url: string };
const draftOf = (value: Connection): Draft => ({backend:value.backend,base_url:value.base_url ?? "",model:value.model,temperature:Number(value.temperature.toFixed(6)),max_tokens:value.max_tokens,thinking:value.thinking,send_local_api_key:value.send_local_api_key ?? false});

export function ModelServiceSettings({token,onChanged}:{token:string;onChanged?:()=>void}) {
  const [data,setData] = useState<Connection | null>(null);
  const [draft,setDraft] = useState<Draft | null>(null);
  const [key,setKey] = useState("");
  const [busy,setBusy] = useState(false);
  const [error,setError] = useState("");
  const [notice,setNotice] = useState("");
  const [confirmClear,setConfirmClear] = useState(false);
  function receive(value:Connection) { setData(value); setDraft(draftOf(value)); }
  useEffect(()=>{
    const controller = new AbortController();
    rpc<Connection>(token,"model/service",{},controller.signal).then(receive).catch(e=>{if(!isAbort(e))setError(errorText(e));});
    return ()=>controller.abort();
  },[token]);
  const dirty = !!(data && draft && JSON.stringify(draftOf(data))!==JSON.stringify(draft));
  async function perform(action:()=>Promise<void>) {
    setBusy(true);setError("");setNotice("");
    try { await action(); } catch(e) { setError(errorText(e)); } finally {setBusy(false);}
  }
  async function save(event:FormEvent) {
    event.preventDefault(); if(!draft)return;
    await perform(async()=>{ receive(await rpc<Connection>(token,"model/update",draft));setNotice("模型配置已保存，后续请求与下次 CLI 启动使用此配置。");onChanged?.(); });
  }
  async function saveKey() {
    await perform(async()=>{ receive(await rpc<Connection>(token,"model/credential",{action:"save",key}));setKey("");setNotice("API Key 已保存，当前 DSH 模型连接已更新。");onChanged?.(); });
  }
  async function clearKey() {
    await perform(async()=>{receive(await rpc<Connection>(token,"model/credential",{action:"clear"}));setKey("");setConfirmClear(false);setNotice("已移除本机保存的密钥并清空当前连接。外部终端环境变量请在终端中另行移除。");onChanged?.();});
  }
  return <div>
    <p className="sc-description">配置 DSH 实际使用的模型服务。设置保存到本机，并用于网页与 CLI 的模型请求。</p>
    {error && <p role="alert" className="sc-info sc-danger">{error}</p>}
    {notice && <p role="status" className="sc-info">{notice}</p>}
    {!draft || !data ? <p className="sc-info">正在读取模型连接…</p> : <>
      <form onSubmit={save}>
        <fieldset disabled={busy} style={{border:0,padding:0,margin:0,minWidth:0}}>
          <label className="sc-field">服务类型<select className="sc-input" value={draft.backend} onChange={event=>{
            const backend=event.target.value; const preset=data.backends.find(item=>item.id===backend);
            setDraft({...draft,backend,base_url:preset?.base_url ?? draft.base_url});setNotice("");
          }}>{data.backends.map(item=><option key={item.id} value={item.id}>{item.label}</option>)}</select></label>
          <label className="sc-field">服务地址<input className="sc-input" type="url" required value={draft.base_url} placeholder="http://127.0.0.1:11434/v1" onChange={event=>{setDraft({...draft,base_url:event.target.value});setNotice("");}} /></label>
          {data.base_url===null && <p className="sc-info">原地址包含内嵌凭据，未向网页展示。请将地址与 API Key 分开填写。</p>}
          <label className="sc-field">模型名称<input className="sc-input" required maxLength={256} value={draft.model} onChange={event=>setDraft({...draft,model:event.target.value})}/></label>
          <label className="sc-field">Temperature<input className="sc-input" type="number" min={0} max={2} step="any" required value={draft.temperature} onChange={event=>setDraft({...draft,temperature:Number(event.target.value)})}/></label>
          <label className="sc-field">最大输出 token<input className="sc-input" type="number" min={1} max={131072} required value={draft.max_tokens} onChange={event=>setDraft({...draft,max_tokens:Number(event.target.value)})}/></label>
          <label className="sc-field"><input type="checkbox" checked={draft.thinking} onChange={event=>setDraft({...draft,thinking:event.target.checked})}/> 启用模型推理（需要模型支持）</label>
          <label className="sc-field"><input type="checkbox" checked={draft.send_local_api_key} onChange={event=>setDraft({...draft,send_local_api_key:event.target.checked})}/> 向本机模型网关发送 API Key</label>
          <p className="sc-info">远程服务使用当前模型密钥；本机免鉴权服务默认不携带密钥。若本机网关要求鉴权，请开启上方选项。更换服务提供方时，请同步更新或移除密钥。</p>
          <div className="sc-actions"><button className="sc-button sc-primary" type="submit" disabled={!dirty}>保存模型配置</button><button className="sc-button" type="button" disabled={!dirty} onClick={()=>{setDraft(draftOf(data));setNotice("");}}>撤销修改</button></div>
        </fieldset>
      </form>
      <section className="sc-section">
        <h3>API Key</h3>
        <p className="sc-description">{data.credential_configured ? "当前连接已配置密钥。" : "当前连接没有密钥，本机免鉴权服务可直接使用。"} 密钥仅写入 DSH 本机凭据文件，不回传、不保存到浏览器。</p>
        <label className="sc-field">新 API Key<input className="sc-input" type="password" autoComplete="new-password" value={key} maxLength={8192} disabled={busy || dirty} onChange={event=>setKey(event.target.value)}/></label>
        {dirty && <p className="sc-info">请先保存或撤销上方连接修改，再管理密钥或测试。</p>}
        <div className="sc-actions"><button className="sc-button" disabled={busy || dirty || !key.trim()} onClick={()=>void saveKey()}>保存 API Key</button><button className="sc-button sc-danger" disabled={busy || dirty || (!data.credential_configured && !data.saved_credential)} onClick={()=>setConfirmClear(true)}>移除 API Key</button></div>
        {confirmClear && <div className="sc-info" role="group" aria-label="确认移除密钥"><p>移除本机保存的密钥，并清空当前 DSH 连接？外部终端中配置的环境变量不会被修改。</p><div className="sc-actions"><button className="sc-button sc-danger" disabled={busy} onClick={()=>void clearKey()}>确认移除</button><button className="sc-button" disabled={busy} onClick={()=>setConfirmClear(false)}>取消</button></div></div>}
      </section>
      <section className="sc-section"><h3>测试实际模型</h3><p className="sc-description">向已保存的服务发送一条 “Reply with OK.” 请求，最多输出 16 个 token。不发送聊天记录；服务可能按实际调用计费。</p><button className="sc-button" disabled={busy || dirty} onClick={()=>void perform(async()=>{const result=await rpc<{ok:boolean;latency_ms:number;message:string}>(token,"model/test");setNotice(`${result.message} 耗时 ${result.latency_ms} ms。`);})}>{busy ? "正在处理…" : "测试模型连接"}</button>{data.fallback_count>0 && <p className="sc-info">当前有 {data.fallback_count} 个备用端点；此测试仅验证上方主服务。</p>}</section>
    </>}
  </div>;
}
