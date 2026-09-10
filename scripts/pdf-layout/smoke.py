import os,sys,json,subprocess,hashlib
from pathlib import Path
if len(sys.argv)!=3: raise SystemExit('usage: smoke.py SERVER_BINARY LOCAL_DOCUMENT')
ROOT=Path(sys.argv[2]).resolve().parent
BINARY=str(Path(sys.argv[1]).resolve())
class Client:
 def __init__(self):
  env=os.environ.copy();env.update(READING_MCP_STATE_DIR='memory',READING_MCP_TELEMETRY='false',READING_MCP_ALLOW_HTTP='false',READING_MCP_LOCAL_ROOTS=str(ROOT))
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
def probe(path):
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
if __name__=='__main__':
 if len(sys.argv)!=3: raise SystemExit('usage: smoke.py SERVER_BINARY LOCAL_DOCUMENT')
 result=probe(Path(sys.argv[2]).resolve())
 print(json.dumps({'sections':len(result['sections']),'exact_checked':result['exact_checked'],'exact_errors':len(result['exact_errors']),'profile':result['opened'].get('reading_profile')},ensure_ascii=False,indent=2))
 if result['exact_errors']: raise SystemExit(1)
