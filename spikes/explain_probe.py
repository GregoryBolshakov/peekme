import json,subprocess,time,select,sys,os,glob
T0=time.time()
def ts(): return f"{(time.time()-T0)*1000:7.0f}ms"
def newest_session():
    fs=glob.glob(os.path.expanduser("~/.codex/sessions/**/*.jsonl"),recursive=True)
    return max(fs,key=os.path.getmtime) if fs else None
before=newest_session(); before_m=os.path.getmtime(before)
p=subprocess.Popen(["codex","app-server"],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL)
def send(o): p.stdin.write((json.dumps(o)+"\n").encode()); p.stdin.flush()
events=[]
def pump(until, timeout=90):
    end=time.time()+timeout
    while time.time()<end:
        r,_,_=select.select([p.stdout],[],[],0.2)
        if not r: continue
        m=json.loads(p.stdout.readline()); m["_t"]=time.time()-T0; events.append(m)
        if "method" in m and "id" in m:      # server -> client request (approval etc.)
            print(ts(),"SERVER REQUEST", m["method"]); send({"id":m["id"],"result":{"decision":"decline"}})
        if until(m): return m
    return None
send({"id":1,"method":"initialize","params":{"clientInfo":{"name":"codex-peek-probe","version":"0.0.1"}}})
pump(lambda m:m.get("id")==1); print(ts(),"initialized")
send({"method":"initialized"})
dev=("You are a peek explainer inside a terminal. The user selected a phrase in an AI coding "
     "assistant's answer and wants a short explanation of what it means in that context. "
     "Do not use tools, do not run commands, do not read files. Answer in plain text, at most 8 lines.")
send({"id":2,"method":"thread/start","params":{"ephemeral":True,"model":"gpt-5.6-luna","cwd":os.getcwd(),
      "sandbox":"read-only","approvalPolicy":"never","developerInstructions":dev,
      "baseInstructions":"You explain short text selections concisely for software developers."}})
r=pump(lambda m:m.get("id")==2); 
if "error" in r: print("thread/start error", r["error"]); p.kill(); sys.exit(1)
th=r["result"]["thread"]["id"]; print(ts(),"thread started", th, "model", r["result"].get("model"), "ephemeral", r["result"]["thread"].get("ephemeral"))
ctx=("Selected text: \"orphaned process group\"\n\n"
     "Surrounding answer:\nUnder any simple wrapper, that stop signal is silently thrown away. Codex calls "
     "kill(0, SIGTSTP), but when Codex is the session leader of its own PTY session its process group is an "
     "orphaned process group, so Linux discards the stop signal and Ctrl+Z becomes a no-op.\n\n"
     "User's question in that turn: \"Analyze problems 1-6 better and search for graceful solutions.\"")
t_turn=time.time()-T0
send({"id":3,"method":"turn/start","params":{"threadId":th,"input":[{"type":"text","text":ctx}],"effort":"low"}})
first=[None]; text=[]
def until(m):
    meth=m.get("method","")
    if meth=="item/agentMessage/delta":
        if first[0] is None: first[0]=m["_t"]
        text.append(m["params"].get("delta",""))
    return meth=="turn/completed"
done=pump(until)
tt=time.time()-T0
kinds=[]
for e in events:
    if e.get("method")=="item/started":
        kinds.append(e["params"]["item"].get("type"))
usage=[e["params"] for e in events if e.get("method")=="thread/tokenUsage/updated"]
print(ts(),"turn completed:", bool(done), "status:", (done or {}).get("params",{}).get("turn",{}).get("status"))
print(f"time to first token: {(first[0]-t_turn)*1000:.0f} ms   total turn: {(tt-t_turn)*1000:.0f} ms" if first[0] else "no delta received")
print("items started:", kinds)
if usage: print("token usage:", json.dumps(usage[-1])[:400])
print("---- explanation ----"); print("".join(text)); print("---------------------")
send({"id":4,"method":"thread/list","params":{"limit":5}})
r=pump(lambda m:m.get("id")==4)
ids=[t["id"] for t in r["result"]["data"]]
print("ephemeral thread in thread/list:", th in ids)
p.kill(); time.sleep(0.5)
after=newest_session()
print("new/modified session file:", (after!=before) or os.path.getmtime(after)!=before_m, after if after!=before else "")
