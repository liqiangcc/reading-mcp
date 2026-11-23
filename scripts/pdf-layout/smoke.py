import argparse,os,sys,json,subprocess,hashlib
from pathlib import Path
parser=argparse.ArgumentParser(description='PDF layout smoke: open, read and re-read every unit')
parser.add_argument('binary')
parser.add_argument('document')
parser.add_argument('--tools',choices=('v1','v2'),default='v1',help='tool set the server exposes')
ARGS=parser.parse_args()
ROOT=Path(ARGS.document).resolve().parent
BINARY=str(Path(ARGS.binary).resolve())
TOOLS=ARGS.tools
class Client:
 def __init__(self):
  env=os.environ.copy();env.update(READING_MCP_STATE_DIR='memory',READING_MCP_TELEMETRY='false',READING_MCP_ALLOW_HTTP='false',READING_MCP_LOCAL_ROOTS=str(ROOT),READING_MCP_TOOLS=TOOLS)
  self.p=subprocess.Popen([BINARY],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,env=env,cwd=ROOT);self.n=0
  self.init=self.req('initialize',{'protocolVersion':'2025-06-18','capabilities':{},'clientInfo':{'name':'md-feasibility','version':'1'}})
  self.p.stdin.write(json.dumps({'jsonrpc':'2.0','method':'notifications/initialized'})+'\n');self.p.stdin.flush()
 def req(self,method,params):
  self.n+=1;self.p.stdin.write(json.dumps({'jsonrpc':'2.0','id':self.n,'method':method,'params':params})+'\n');self.p.stdin.flush()
  for line in self.p.stdout:
   r=json.loads(line)
   if r.get('id')==self.n:
    if 'error' in r:raise RuntimeError(r)
    return r['result']
  raise RuntimeError('closed')
 def call(self,name,args):
  r=self.req('tools/call',{'name':name,'arguments':args})
  if r.get('isError'):raise RuntimeError(r)
  if r.get('structuredContent') is not None:return r['structuredContent']
  return json.loads(next(c['text'] for c in r['content'] if c['type']=='text'))
 def close(self):self.p.stdin.close();self.p.wait(timeout=10)
def section_of(handle):return '.'.join(handle.split('.')[:2])
def probe_v1(path):
 c=Client()
 try:
  op=c.call('open_document',{'source':path.as_uri()});did=op['document_id']
  structure=c.call('get_document_structure',{'document_id':did,'max_nodes':200})
  assert structure['complete'] and not structure['truncated'], 'incomplete structure'
  all_items=[];sections=[];seen=set();exact_errors=[]
  def walk(nodes):
   for node in nodes:
    yield node
    yield from walk(node.get('children',[]))
  for section in walk(structure['sections']):
   sid=section['section_id'];args={'document_id':did,'section_id':sid,'requested_kind':'sentence','coverage_policy':'preserve_source','max_items':200,'max_chars':65536}
   units=[];seen_cursors=set()
   while True:
    u=c.call('get_text_units',args);units.extend(u['items'])
    cursor=u.get('next_cursor') or u.get('stream',{}).get('next_cursor')
    if not cursor:
     assert u['complete'], 'missing continuation with incomplete units'
     break
    assert cursor not in seen_cursors, 'repeated continuation cursor'
    seen_cursors.add(cursor);args['cursor']=cursor
   sections.append({'node':section,'items':units,'coverage':u['coverage'],'stream':u['stream']})
   for item in units:
    key=json.dumps(item['locator'],sort_keys=True)
    if key in seen:continue
    seen.add(key);all_items.append(item)
    exact=c.call('read_document',{'document_id':did,'target_locator':item['locator'],'max_chars':65536})
    keys=['document_id','content_hash','normalized_document_hash','owner_section_id','normalized_range','native_location','section_path']
    consistent=exact.get('content')==item['text'] and exact.get('complete') and not exact.get('truncated') and exact.get('resolved_target_locator')==item['locator'] and all(exact.get('returned_locator',{}).get(k)==item['locator'].get(k) for k in keys)
    if not consistent:exact_errors.append({'item':item,'exact':exact})
  return {'input':str(path),'opened':op,'structure':structure,'sections':sections,'unique_items':all_items,'exact_checked':len(all_items),'exact_errors':exact_errors}
 finally:c.close()
def probe_v2(path):
 c=Client()
 try:
  op=c.call('open',{'source':path.as_uri()});did=op['doc']
  nodes=[];cursor=None;seen_cursors=set()
  while True:
   args={'at':did,'depth':32,'limit':1000}
   if cursor is not None:args['cursor']=cursor
   outline=c.call('outline',args)
   for item in outline.get('items') or []:
    assert isinstance(item,list) and item and isinstance(item[0],str),'outline item without a handle'
    nodes.append({'section_id':item[0],'title':item[1] if len(item)>1 else ''})
   cursor=outline.get('next')
   if not cursor:break
   assert cursor not in seen_cursors, 'repeated outline cursor'
   seen_cursors.add(cursor)
  by_section={node['section_id']:[] for node in nodes};order=[node['section_id'] for node in nodes]
  from_handle=did;seen_next=set()
  while True:
   batch=c.call('read',{'from':from_handle,'max_chars':16000})
   for item in batch.get('items') or []:
    handle=item[0];sid=section_of(handle)
    by_section.setdefault(sid,[]).append({'handle':handle,'text':item[1] if len(item)>1 else '','kind':item[2] if len(item)>2 else None})
    if sid not in order:order.append(sid)
   nxt=batch.get('next')
   if not nxt:break
   assert nxt not in seen_next, 'repeated read continuation'
   seen_next.add(nxt);from_handle=nxt
  sections=[{'node':{'section_id':sid},'items':by_section.get(sid,[]),'coverage':None,'stream':None} for sid in order]
  all_items=[];seen=set();exact_errors=[]
  for section in sections:
   for item in section['items']:
    if item['handle'] in seen:continue
    seen.add(item['handle']);all_items.append(item)
    exact=c.call('read',{'from':item['handle']})
    consistent=(exact.get('text')==item['text']
                and exact.get('at') in (None,item['handle'])
                and (exact.get('section') is None or exact['section'].get('s')==section['node']['section_id']))
    if not consistent:exact_errors.append({'item':item,'exact':exact})
  structure={'sections':nodes,'complete':True,'truncated':False}
  return {'input':str(path),'opened':op,'structure':structure,'sections':sections,'unique_items':all_items,'exact_checked':len(all_items),'exact_errors':exact_errors}
 finally:c.close()
if __name__=='__main__':
 result=(probe_v1 if TOOLS=='v1' else probe_v2)(Path(ARGS.document).resolve())
 print(json.dumps({'sections':len(result['sections']),'exact_checked':result['exact_checked'],'exact_errors':len(result['exact_errors']),'profile':result['opened'].get('reading_profile')},ensure_ascii=False,indent=2))
 if result['exact_errors']: raise SystemExit(1)
