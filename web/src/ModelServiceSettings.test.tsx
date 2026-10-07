import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ModelServiceSettings } from "./ModelServiceSettings";
import { rpc } from "./api";
vi.mock("./api",async(original)=>({...await original<typeof import("./api")>(),rpc:vi.fn()}));
const initial={backend:"deepseek",base_url:"https://api.deepseek.com",model:"deepseek-chat",temperature:0.6,max_tokens:4096,thinking:false,credential_configured:true,saved_credential:true,ready:true,fallback_count:0,backends:[{id:"deepseek",label:"deepseek",base_url:"https://api.deepseek.com",requires_key:true},{id:"ollama",label:"ollama",base_url:"http://127.0.0.1:11434/v1",requires_key:false}]};
beforeEach(()=>{vi.mocked(rpc).mockReset();vi.mocked(rpc).mockImplementation(async(_token,method,params)=>method==="model/update"?{...initial,...params}:initial);});
afterEach(cleanup);
describe("DSH model service",()=>{
  it("saves the real provider, endpoint and model through authenticated RPC",async()=>{
    const changed=vi.fn();render(<ModelServiceSettings token="token" onChanged={changed}/>);
    await screen.findByLabelText("服务类型");
    fireEvent.change(screen.getByLabelText("服务类型"),{target:{value:"ollama"}});
    expect((screen.getByLabelText("服务地址") as HTMLInputElement).value).toBe("http://127.0.0.1:11434/v1");
    fireEvent.change(screen.getByLabelText("模型名称"),{target:{value:"qwen3:8b"}});
    expect((screen.getByRole("button",{name:"测试模型连接"}) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("button",{name:"保存模型配置"}));
    await waitFor(()=>expect(rpc).toHaveBeenCalledWith("token","model/update",expect.objectContaining({backend:"ollama",base_url:"http://127.0.0.1:11434/v1",model:"qwen3:8b"})));
    await waitFor(()=>expect(changed).toHaveBeenCalledOnce());
  });
  it("surfaces backend failures without claiming saved or testing draft connections",async()=>{
    vi.mocked(rpc).mockImplementation(async(_token,method)=>{if(method==="model/update")throw Error("write failed");return initial;});
    render(<ModelServiceSettings token="token"/>);await screen.findByLabelText("模型名称");
    fireEvent.change(screen.getByLabelText("模型名称"),{target:{value:"new-model"}});
    fireEvent.click(screen.getByRole("button",{name:"保存模型配置"}));
    expect((await screen.findByRole("alert")).textContent).toContain("write failed");
    expect(screen.queryByRole("status")).toBeNull();
  });
  it("does not test automatically and clears the password input after saving",async()=>{
    render(<ModelServiceSettings token="token"/>);await screen.findByLabelText("新 API Key");
    expect(vi.mocked(rpc).mock.calls.some(call=>call[1]==="model/test")).toBe(false);
    fireEvent.change(screen.getByLabelText("新 API Key"),{target:{value:"fixture-only-key"}});
    fireEvent.click(screen.getByRole("button",{name:"保存 API Key"}));
    await waitFor(()=>expect((screen.getByLabelText("新 API Key") as HTMLInputElement).value).toBe(""));
    expect(rpc).toHaveBeenCalledWith("token","model/credential",{action:"save",key:"fixture-only-key"});
    fireEvent.click(screen.getByRole("button",{name:"移除 API Key"}));
    fireEvent.click(screen.getByRole("button",{name:"取消"}));
    expect(vi.mocked(rpc).mock.calls.some(call=>call[2]?.action==="clear")).toBe(false);
  });
});
