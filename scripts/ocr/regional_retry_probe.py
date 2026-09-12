"""Diagnostic regional PSM6 retry; never used by production runtime."""
import hashlib, json, re, sys, unicodedata
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
                psm3=ns["ocr_page"](page); psm3=psm3 or []
                boxes=psm3[:]; conflicts=[]
                for i,a in enumerate(boxes):
                    for j,b in enumerate(boxes[i+1:],i+1):
                        if min(area(a["bbox"]),area(b["bbox"])) and overlap(a["bbox"],b["bbox"])/min(area(a["bbox"]),area(b["bbox"]))>=.9: conflicts.append((i,j))
                components=[]
                for i,j in conflicts:
                    merged=next((c for c in components if i in c or j in c),None)
                    if merged is None: components.append({i,j})
                    else: merged.update((i,j))
                page_diag={"psm3_boxes":boxes,"conflicts":conflicts,"components":[],"psm6_boxes":[]}
                if components:
                    config6={**config,"psm":6}; ns["OCR_CONFIG"]=config6; psm6=ns["ocr_page"](page) or []; ns["OCR_CONFIG"]=config
                    page_diag["psm6_boxes"]=psm6; replacements=[]
                    for component in components:
                        roi=[min(boxes[i]["bbox"][k] for i in component) for k in (0,1)]+[max(boxes[i]["bbox"][k] for i in component) for k in (2,3)]
                        candidates=[b for b in psm6 if inside(b["bbox"],roi)]
                        resolved=bool(candidates)
                        page_diag["components"].append({"indices":sorted(component),"roi":roi,"candidate_count":len(candidates),"resolved":resolved})
                        if resolved: replacements.append((min(component),component,candidates))
                    for first,component,candidates in sorted(replacements,reverse=True):
                        boxes[first:first+1]=candidates; del boxes[first+1:first+1+len(component)-1]
                page_layout["boxes"]=boxes; item["pages"].append(page_diag)
            result=ns["project"](layout)
        gold=json.loads((root/"gold"/(case+".json")).read_text()); paragraphs=[b["text"] for s in result["sections"] for b in s["blocks"] if b["kind"]=="paragraph"]; text="\n\n".join(paragraphs)
        item.update({"canonical_text":text,"cer":metric(gold["text"],text),"paragraph_count":len(paragraphs),"paragraphs":paragraphs,"ocr_evidence":result.get("ocr_evidence",[])})
        report["cases"][case]=item
    out=Path(sys.argv[1]) if len(sys.argv)>1 else Path("/tmp/regional-retry-probe.json"); out.parent.mkdir(parents=True,exist_ok=True); rendered=json.dumps(report,ensure_ascii=False,indent=2)+"\n"; out.write_text(rendered); print(rendered,end="")
if __name__=="__main__": main()
