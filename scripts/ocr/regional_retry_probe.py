"""Diagnostic regional PSM6 retry; never used by production runtime."""
import copy, json, re, sys, unicodedata, statistics
from pathlib import Path

def norm(s): return " ".join(unicodedata.normalize("NFC", s).split())
def dist(a,b):
    row=list(range(len(b)+1))
    for i,x in enumerate(a,1):
        nxt=[i]
        for j,y in enumerate(b,1): nxt.append(min(nxt[-1]+1,row[j]+1,row[j-1]+(x!=y)))
        row=nxt
    return row[-1]
def metric(a,b):
    a,b=norm(a),norm(b); d=dist(a,b); return {"errors":d,"denominator":len(a),"rate":d/len(a) if a else None}
def area(b): return max(0,b[2]-b[0])*max(0,b[3]-b[1])
def overlap(a,b):
    x=max(0,min(a[2],b[2])-max(a[0],b[0])); y=max(0,min(a[3],b[3])-max(a[1],b[1])); return x*y
def inside(b,r): return b[0]>=r[0] and b[1]>=r[1] and b[2]<=r[2] and b[3]<=r[3]
def line_boxes(box): return [line["bbox"] for line in box.get("textlines", [])]
def adjacent(a,b,gap):
    for x in line_boxes(a):
        for y in line_boxes(b):
            vertical=max(0,min(x[3],y[3])-max(x[1],y[1]))
            horizontal=max(0,max(x[0],y[0])-min(x[2],y[2]))
            if vertical >= min(x[3]-x[1], y[3]-y[1])*.5 and horizontal <= gap: return True
    return False
def merge_components(pairs):
    parent={i:i for pair in pairs for i in pair}
    def find(x):
        while parent[x]!=x: parent[x]=parent[parent[x]]; x=parent[x]
        return x
    for a,b in pairs:
        ra,rb=find(a),find(b)
        if ra!=rb: parent[rb]=ra
    groups={}
    for i in parent: groups.setdefault(find(i),set()).add(i)
    return list(groups.values())
def rebuild_boxes(original, replacements):
    seen = set()
    for first, members, _ in replacements:
        if first != min(members): raise ValueError("replacement must be inserted at component minimum")
        if seen & members: raise ValueError("overlapping replacement components")
        seen.update(members)
    by_index={first:(members,candidates) for first,members,candidates in replacements}
    members=set().union(*(m for _,m,_ in replacements)) if replacements else set()
    out=[]
    for i,box in enumerate(original):
        if i in by_index: out.extend(by_index[i][1])
        if i not in members: out.append(box)
    return out
def close_components(components):
    groups=[set(c) for c in components]
    changed=True
    while changed:
        changed=False
        for i in range(len(groups)):
            for j in range(i+1,len(groups)):
                if groups[i] & groups[j]: groups[i].update(groups.pop(j)); changed=True; break
            if changed: break
    return groups

def main():
    import pymupdf, pymupdf4llm
    root=Path("tests/fixtures/scanned_pdf"); ns={}; exec(Path("src/parsing/pdf_layout_worker.py").read_text(),ns)
    base={"enabled":True,"engine_path":"/usr/bin/tesseract","tessdata_path":"/usr/share/tesseract-ocr/5/tessdata","languages":[],"operator_revision":"1","dpi":300,"oem":1,"psm":3,"detector_version":"pdf-layout/v1","protocol_version":"pdf-layout/v1"}
    report={"schema":"ocr-regional-retry-probe/v1","cases":{}}
    for case in ("F02","F06","F07","F08","F14"):
        config={**base,"languages":["chi_sim"] if case=="F07" else (["eng","chi_sim"] if case=="F08" else ["eng"])}; ns["OCR_CONFIG"]=config
        item={"config":config,"pages":[]}
        with pymupdf.open(root/"pdf"/(case+".pdf")) as doc:
            pymupdf4llm.use_layout(True); layout=json.loads(pymupdf4llm.to_json(doc,use_ocr=False))
            for page,page_layout in zip(doc,layout["pages"]):
                selected, diagnostic = ns["_regional_ocr"](page)
                page_layout["boxes"] = selected
                item["pages"].append(diagnostic)
            result=ns["project"](layout)
        gold=json.loads((root/"gold"/(case+".json")).read_text()); paragraphs=[b["text"] for s in result["sections"] for b in s["blocks"] if b["kind"]=="paragraph"]; text="\n\n".join(paragraphs)
        item.update({"canonical_text":text,"cer":metric(gold["text"],text),"paragraph_count":len(paragraphs),"paragraphs":paragraphs,"ocr_evidence":result.get("ocr_evidence",[])})
        report["cases"][case]=item
    out=Path(sys.argv[1]) if len(sys.argv)>1 else Path("/tmp/regional-retry-probe.json"); out.parent.mkdir(parents=True,exist_ok=True); rendered=json.dumps(report,ensure_ascii=False,indent=2)+"\n"; out.write_text(rendered); print(rendered,end="")
if __name__=="__main__": main()
