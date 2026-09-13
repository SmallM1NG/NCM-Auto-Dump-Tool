import React,{useCallback,useEffect,useRef,useState}from'react';import{createRoot}from'react-dom/client';import{getCurrentWindow}from'@tauri-apps/api/window';import{invoke}from'@tauri-apps/api/core';import{listen}from'@tauri-apps/api/event';import{open}from'@tauri-apps/plugin-dialog';import{Activity,FolderOpen,Upload,FileAudio,FileText}from'lucide-react';import logoUrl from'./assets/NADT.png';import'./app.css';import'./theme.css';
const win=getCurrentWindow();
const IconMin=()=> <svg className="win-icon" viewBox="0 0 12 12" aria-hidden="true"><rect x="1.5" y="5.5" width="9" height="1"/></svg>;
const IconMax=()=> <svg className="win-icon" viewBox="0 0 12 12" aria-hidden="true"><rect x="1.5" y="1.5" width="9" height="9" fill="none" stroke="currentColor" strokeWidth="1"/></svg>;
const IconRestore=()=> <svg className="win-icon" viewBox="0 0 12 12" aria-hidden="true"><rect x="1.5" y="3.5" width="7" height="7" fill="none" stroke="currentColor" strokeWidth="1"/><path d="M3.5 3.5V1.5h7v7h-2" fill="none" stroke="currentColor" strokeWidth="1"/></svg>;
const IconClose=()=> <svg className="win-icon" viewBox="0 0 12 12" aria-hidden="true"><path d="M2 2l8 8M10 2l-8 8" stroke="currentColor" strokeWidth="1.2" fill="none"/></svg>;

const EMPTY={download_dir:'',output_dir:'',enable_notification:true,enable_sound:true,enable_tray_flash:true,minimal_metadata:false,embed_lrc:false,shred_mode:false,first_run:false,always_on_top:false,close_to_tray:false,silent_start:false,theme:'system',total_processed:0,window:null};

function App(){
  const[cfg,setCfg]=useState(EMPTY);
  const[running,setRunning]=useState(false),[maximized,setMaximized]=useState(false),[logs,setLogs]=useState([]),[starMsg,setStarMsg]=useState(0),[autoScroll,setAutoScroll]=useState(true),[queue,setQueue]=useState({waiting:0,processed:0,file:'暂无',status:'空闲',progress:0}),[total,setTotal]=useState(0),[version,setVersion]=useState(''),[dragging,setDragging]=useState(false),[dropInfo,setDropInfo]=useState(null),[dropShown,setDropShown]=useState(false);
  const latest=useRef(EMPTY);
  const dirty=useRef(false);
  const themeNames={dark:'深色模式',light:'浅色模式',system:'跟随系统'};
  const themeOrder=['dark','light','system'];
  const theme=themeNames[cfg.theme]||themeNames.dark;
  useEffect(()=>{
    const mode=cfg.theme||'dark';
    const media=window.matchMedia('(prefers-color-scheme: dark)');
    const apply=()=>{document.documentElement.dataset.theme=mode==='system'?(media.matches?'dark':'light'):mode};
    apply();
    if(mode!=='system')return;
    media.addEventListener?.('change',apply);
    return()=>media.removeEventListener?.('change',apply);
  },[cfg.theme]);

  const persist=useCallback(async next=>{
    dirty.current=false;
    try{await invoke('save_config',{config:next})}catch(e){console.error(e)}
  },[]);

  /** Persist pending edits, typically when an input loses focus. */
  const flush=useCallback(()=>{
    if(dirty.current)persist(latest.current);
  },[]);

  const ext=p=>String(p).split('.').pop().toLowerCase();
  // Keep the overlay mounted while it fades out, then drop it. `dragging`
  // toggles the shown class, `mounted` controls whether it exists at all.
  const[dropMounted,setDropMounted]=useState(false);
  useEffect(()=>{
    if(dragging){setDropMounted(true);const id=requestAnimationFrame(()=>setDropShown(true));return()=>cancelAnimationFrame(id)}
    setDropShown(false);
    const t=setTimeout(()=>setDropMounted(false),150);
    return()=>clearTimeout(t);
  },[dragging]);  /** Split hovered/dropped paths into the ones we act on and the ones we skip. */
  const classify=paths=>{
    const ncm=[], lrc=[], other=[];
    for(const p of paths){const e=ext(p);if(e==='ncm')ncm.push(p);else if(e==='lrc')lrc.push(p);else other.push(p)}
    return {ncm,lrc,other,total:paths.length};
  };
  const handleDrop=async paths=>{
    const {ncm,lrc,other}=classify(paths);
    if(other.length){
      setLogs(xs=>[...xs,`[${new Date().toLocaleTimeString('zh-CN',{hour12:false})}] [ERROR] 已忽略 ${other.length} 个不支持的文件`].slice(-200));
    }
    if(lrc.length){
      setLogs(xs=>[...xs,`[${new Date().toLocaleTimeString('zh-CN',{hour12:false})}] [INFO] 收到 ${lrc.length} 个 LRC，将随同名 NCM 自动使用`].slice(-200));
    }
    if(!ncm.length){
      if(!other.length&&lrc.length){
        setLogs(xs=>[...xs,`[${new Date().toLocaleTimeString('zh-CN',{hour12:false})}] [ERROR] 只收到 LRC，请同时拖入对应的 NCM 文件`].slice(-200));
      }
      return;
    }
    try{await invoke('enqueue_files',{paths:ncm})}catch(e){
      setLogs(xs=>[...xs,`[${new Date().toLocaleTimeString('zh-CN',{hour12:false})}] [ERROR] ${String(e)}`].slice(-200));
    }
  };

  useEffect(()=>{
     // Every listener is collected so cleanup cannot miss one.
     const unlisteners=[];
     listen('log-message',e=>setLogs(xs=>[...xs,String(e.payload)].slice(-200))).then(f=>unlisteners.push(f));
     listen('queue-state',e=>setQueue(q=>({...q,...e.payload}))).then(f=>unlisteners.push(f));
     listen('queue-progress',e=>{const x=e.payload||{};if(typeof x.total==='number'&&x.total>0){setTotal(x.total);latest.current.total_processed=x.total}setQueue(q=>{const done=x.state==='completed'||x.state==='failed';return{...q,waiting:x.queued??q.waiting,processed:typeof x.processed==='number'?x.processed:q.processed,file:x.file||q.file,status:x.status||q.status,progress:done?0:(typeof x.progress==='number'?x.progress:q.progress)}})}).then(f=>unlisteners.push(f));
     // Native file drag-and-drop: Tauri reports the hovered paths, and the
     // overlay only stays up while the pointer is over the window.
     const onDragEnter=e=>{setDragging(true);setDropInfo(classify(e.payload?.paths||[]))};
     listen('tauri://drag-enter',onDragEnter).then(f=>unlisteners.push(f));
     listen('tauri://drag-over',onDragEnter).then(f=>unlisteners.push(f));
     listen('tauri://drag-leave',()=>{setDragging(false);setDropInfo(null)}).then(f=>unlisteners.push(f));
     listen('tauri://drag-drop',e=>{setDragging(false);setDropInfo(null);handleDrop(e.payload?.paths||[])}).then(f=>unlisteners.push(f));
    invoke('get_config').then(x=>{
      setTotal(x.total_processed||0);
      const loaded={...EMPTY,total_processed:x.total_processed||0,window:x.window??null,download_dir:x.download_dir||'',output_dir:x.output_dir||'',enable_notification:x.enable_notification??true,enable_sound:x.enable_sound!==false,enable_tray_flash:x.enable_tray_flash??true,embed_lrc:!!x.embed_lrc,shred_mode:!!x.shred_mode,always_on_top:!!x.always_on_top,close_to_tray:!!x.close_to_tray,silent_start:!!x.silent_start,theme:['dark','light','system'].includes(x.theme)?x.theme:'system',minimal_metadata:!!x.minimal_metadata};
      latest.current=loaded;
      setCfg(loaded);
    }).catch(console.error);
    win.isMaximized().then(setMaximized).catch(()=>{});
    // A silent start begins monitoring before this window is even shown.
    invoke('is_monitoring').then(v=>{if(v)setRunning(true)}).catch(()=>{});
    // Backfill anything logged before the listener above was attached, while
    // keeping any lines that arrived in the meantime.
    invoke('get_logs').then(xs=>{if(Array.isArray(xs)&&xs.length)setLogs(cur=>[...xs,...cur].slice(-200))}).catch(()=>{});
    import('@tauri-apps/api/app').then(m=>m.getVersion()).then(setVersion).catch(()=>{});
    const un=win.onResized(()=>{win.isMaximized().then(setMaximized).catch(()=>{})});
    return()=>{un.then(f=>f()).catch(()=>{});unlisteners.forEach(u=>{try{u()}catch{}})};
  },[]);

  const logEnd=useRef(null);
  useEffect(()=>{if(autoScroll&&logEnd.current)logEnd.current.scrollIntoView({block:'end'})},[logs,autoScroll]);
  const starMessages=['By 小小小小铭','如果喜欢可以给个 Star 谢谢喵',`NADT 已为你处理了 ${total} 首曲目`,'可手动拖拽 NCM 文件至界面'];
  useEffect(()=>{const timer=setInterval(()=>setStarMsg(i=>(i+1)%starMessages.length),5000);return()=>clearInterval(timer)},[starMessages.length]);

  const update=(patch,immediate)=>{
    // Compute the next config outside the state updater: running side effects    // inside a setState updater is unsafe (it may run twice in StrictMode, and
    // it executes during render rather than in an effect).
    const next={...latest.current,...patch};
    latest.current=next;
    setCfg(next);
    if(immediate){persist(next)}else{dirty.current=true}
  };

  const Hint=({text})=><span className="hint" tabIndex={0} onClick={e=>e.preventDefault()}><svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="7" fill="none" stroke="currentColor" strokeWidth="1.2"/><path d="M8 11.6v.01" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"/><path d="M6.2 6.1a1.8 1.8 0 1 1 2.4 1.7c-.4.15-.6.5-.6.9v.4" fill="none" stroke="currentColor" strokeWidth="1.2" strokeLinecap="round"/></svg><span className="tip">{text}</span></span>;
  const C=({t,v,x,disabled,hint})=><label className={`check${disabled?' disabled':''}`}><input type="checkbox" checked={v} disabled={disabled} onChange={e=>x(e.target.checked)}/><span>{t}</span>{hint&&<Hint text={hint}/>}</label>;
  const browse=async key=>{
    const p=await open({directory:true,multiple:false});
    if(!p)return;
    // 下载目录必须解析为真实的 VipSongsDownload；选择父级根目录时
    // 自动定位其下的同名文件夹，解析失败则不保存错误路径。
    if(key==='download_dir'){
      try{
        const resolved=await invoke('resolve_download_dir',{path:p});
        if(!resolved){appendError('所选目录无效：请选择包含 VipSongsDownload 的下载根目录，或直接选择 VipSongsDownload 文件夹');return}
        update({[key]:resolved},true);
        setLogs(xs=>[...xs,`[${new Date().toLocaleTimeString('zh-CN',{hour12:false})}] [INFO] 监控目录: ${resolved}`].slice(-200));
      }catch(e){appendError(e)}
    }else{
      update({[key]:p},true);
    }
  };
  const appendError=e=>setLogs(xs=>[...xs,`[${new Date().toLocaleTimeString('zh-CN',{hour12:false})}] [ERROR] ${String(e)}`].slice(-200));
  const start=async()=>{
    // Keep the session tally: only the pending depth and progress reset.
    try{await invoke('save_config',{config:latest.current});setQueue(q=>({...q,waiting:0,file:'暂无',status:'空闲',progress:0}));await invoke('start_monitor');setRunning(true)}catch(e){appendError(e)}
  };
  const stop=async()=>{
    try{await invoke('stop_monitor');setRunning(false);setQueue(q=>({...q,waiting:0,file:'暂无',status:'已停止',progress:0}))}catch(e){appendError(e)}  };
  const toggle=()=>running?stop():start();
  const act=fn=>e=>{e.preventDefault();e.stopPropagation();fn()};

  return <div className="app-shell">
    <div className="titlebar" data-tauri-drag-region onMouseDown={e=>{
      // 只处理单击拖动。双击必须让给 Tauri 自己的处理：它在 document 上
      // 监听 mousedown，detail===2 时调用 internal_toggle_maximize。冒泡顺序是
      // 先本元素后 document，所以这里若不主动让开，startDragging() 会和最大化
      // 同时触发，窗口先变大再被拖动逻辑拉回，看起来就是闪一下。
      if(e.button!==0||e.detail!==1)return;
      if(e.target.closest('.window-actions'))return;
      win.startDragging();
    }}>
      <div className="brand"><img className="brand-logo" src={logoUrl} alt="NADT"/><span className="brand-title">NADT</span><span className="brand-version">v{version}</span></div>
      <div className="titlebar-right">
        <div className="window-actions">
          <button type="button" className="win-btn" aria-label="最小化" onClick={act(()=>win.minimize())}><IconMin/></button>
          <button type="button" className="win-btn" aria-label={maximized?'还原':'最大化'} onClick={act(()=>win.toggleMaximize())}>{maximized?<IconRestore/>:<IconMax/>}</button>
          <button type="button" className={`win-btn${cfg.close_to_tray?'':' close'}`} aria-label={cfg.close_to_tray?'最小化到托盘':'关闭'} onClick={act(()=>{flush();if(cfg.close_to_tray){invoke('hide_window').catch(console.error)}else{invoke('quit_app').catch(console.error)}})}><IconClose/></button>
        </div>
      </div>
    </div>
    <main className="window">
      <section className="card directories"><h2>目录设置</h2>
        <label>网易云下载目录<div className="path"><input value={cfg.download_dir} onChange={e=>update({download_dir:e.target.value})} onBlur={flush} placeholder="选择下载目录"/><button className="browse-btn" onClick={()=>browse('download_dir')}><FolderOpen size={13} strokeWidth={2.25} aria-hidden="true"/>浏览</button></div></label>
        <label>输出目录<div className="path"><input value={cfg.output_dir} onChange={e=>update({output_dir:e.target.value})} onBlur={flush} placeholder="选择输出目录"/><button className="browse-btn" onClick={()=>browse('output_dir')}><FolderOpen size={13} strokeWidth={2.25} aria-hidden="true"/>浏览</button></div></label>
      </section>
      <div className="settings-grid">
        <section className="card"><h2>通知设置</h2>
          <C t="启用系统通知" v={cfg.enable_notification} x={v=>update({enable_notification:v},true)}/>
          <C t="启用系统通知提示音" v={cfg.enable_sound} x={v=>update({enable_sound:v},true)} disabled={!cfg.enable_notification}/>
          <C t="启用托盘图标闪烁" v={cfg.enable_tray_flash} x={v=>update({enable_tray_flash:v},true)}/>
        </section>
        <section className="card"><h2>输出设置</h2>
          <C t="极简模式" hint="Tag只保留title artist cover lyric字段" v={cfg.minimal_metadata} x={v=>update({minimal_metadata:v},true)}/>
          <C t="写入 LRC 歌词文件" v={cfg.embed_lrc} x={v=>update({embed_lrc:v},true)}/>
          <C t="删除原始文件" v={cfg.shred_mode} x={v=>update({shred_mode:v},true)}/>
        </section>
      </div>
      <section className="card"><h2>软件设置</h2>
        <div className="settings-row">
          <C t="窗口置顶" v={cfg.always_on_top} x={v=>{update({always_on_top:v},true);invoke('set_always_on_top',{enabled:v}).catch(console.error)}}/>
          <C t="托盘驻留" v={cfg.close_to_tray} x={v=>update({close_to_tray:v},true)}/>
          <C t="静默启动" hint="以托盘模式启动，并自动启动监控" v={cfg.silent_start} x={v=>update({silent_start:v},true)}/>
           <button type="button" className="theme-button" onClick={()=>{const next=themeOrder[(themeOrder.indexOf(cfg.theme)+1)%themeOrder.length];update({theme:next},true)}}>{theme}</button>
        </div>
      </section>
      <section className="card log-card"><div className="log-head"><h2>运行日志</h2><label className="log-auto"><input type="checkbox" checked={autoScroll} onChange={e=>setAutoScroll(e.target.checked)}/><span>自动滚动</span></label></div><pre className="log-box">{logs.length?logs.map((line,i)=><React.Fragment key={i}><span className={line.includes('[ERROR]')?'log-error':''}>{line}</span>{i<logs.length-1?'\n':''}</React.Fragment>):'暂无日志'}<span ref={logEnd}/></pre></section>
      <section className="card queue-card"><div className="queue-head"><h2>队列状态</h2><span className="queue-counts"><b>{queue.waiting}</b> 等待 / <b>{queue.processed}</b> 已完成</span></div><div className="queue-current"><span className="queue-file" title={queue.file}>{queue.file}</span><b className="queue-status" title={queue.status}>{queue.status}</b></div><div className="progress-track"><i style={{width:`${queue.progress}%`}}/></div></section>
      <footer>
        <a className="gh-link" href="https://github.com/SmallM1NG" target="_blank" rel="noreferrer" onClick={e=>{e.preventDefault();invoke('open_url',{url:'https://github.com/SmallM1NG'})}} aria-label="GitHub"><svg viewBox="0 0 16 16" aria-hidden="true"><path fill="currentColor" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z"/></svg><span key={starMsg} className="star-message">{starMessages[starMsg]}</span></a>
        <div className="footer-actions"><button className="secondary" onClick={()=>invoke('clear_notification_registry').catch(e=>appendError(e))}><Hint text="清除系统通知注册表"/>扫地出门</button><button className={running?'danger':'primary'} onClick={toggle}><Activity className={`mon-icon${running?' beating':''}`} size={14} strokeWidth={2.25} aria-hidden="true"/>{running?'停止监控':'启用监控'}</button></div>
      </footer>
    </main>
    {dropMounted&&<div className={`drop-overlay${dropShown?' shown':''}`}>
      <div className="drop-card">
        <Upload className="drop-icon" size={38} strokeWidth={1.5} aria-hidden="true"/>
        <div className="drop-title">松开以加入队列</div>
        <div className="drop-hint">支持 .ncm 文件，同名 .lrc 会自动写入歌词</div>
        {dropInfo&&dropInfo.total>0&&<div className="drop-list">
          {dropInfo.ncm.length>0&&<span className="drop-tag ok"><FileAudio size={12} aria-hidden="true"/>{dropInfo.ncm.length} 个 NCM</span>}
          {dropInfo.lrc.length>0&&<span className="drop-tag ok"><FileText size={12} aria-hidden="true"/>{dropInfo.lrc.length} 个 LRC</span>}
          {dropInfo.other.length>0&&<span className="drop-tag bad">{dropInfo.other.length} 个不支持</span>}
        </div>}
      </div>
    </div>}
  </div>
}
createRoot(document.getElementById('root')).render(<App/>);

