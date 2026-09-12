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
    by_index={first:(members,candidates) for first,members,candidates in replacements}
    members=set().union(*(m for _,m,_ in replacements)) if replacements else set()
    out=[]
    for i,box in enumerate(original):
        if i in by_index: out.extend(by_index[i][1])
        if i not in members: out.append(box)
    return out

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
                original_boxes=copy.deepcopy(psm3); boxes=psm3[:]; conflicts=[]
                for i,a in enumerate(boxes):
                    for j,b in enumerate(boxes[i+1:],i+1):
                        if min(area(a["bbox"]),area(b["bbox"])) and overlap(a["bbox"],b["bbox"])/min(area(a["bbox"]),area(b["bbox"]))>=.9: conflicts.append((i,j))
                components=merge_components(conflicts)
                heights=[line["bbox"][3]-line["bbox"][1] for box in boxes for line in box.get("textlines", []) if line["bbox"][3]>line["bbox"][1]]
                median_height=statistics.median(heights) if heights else 0
                expanded=True
                while expanded:
                    expanded=False
                    for component in components:
                        for index, box in enumerate(boxes):
                            if index not in component and any(adjacent(boxes[member], box, median_height) for member in component):
                                component.add(index); expanded=True
                page_diag={"psm3_boxes":copy.deepcopy(original_boxes),"conflicts":conflicts,"components":[],"psm6_boxes":[]}
                if components:
                    config6={**config,"psm":6}; ns["OCR_CONFIG"]=config6; psm6=ns["ocr_page"](page) or []; ns["OCR_CONFIG"]=config
                    page_diag["psm6_boxes"]=psm6; replacements=[]
                    replacements=[]; tolerance=2*72/config["dpi"]
                    page_rect=[0,0,page.rect.width,page.rect.height]
                    for component in components:
                        raw_roi=[min(original_boxes[i]["bbox"][k] for i in component) for k in (0,1)]+[max(original_boxes[i]["bbox"][k] for i in component) for k in (2,3)]
                        roi=[max(page_rect[0],raw_roi[0]-tolerance),max(page_rect[1],raw_roi[1]-tolerance),min(page_rect[2],raw_roi[2]+tolerance),min(page_rect[3],raw_roi[3]+tolerance)]
                        candidates=[b for b in psm6 if inside(b["bbox"],roi)]
                        centers=[((s["bbox"][0]+s["bbox"][2])/2,(s["bbox"][1]+s["bbox"][3])/2) for i in component for l in original_boxes[i].get("textlines",[]) for s in l.get("spans",[])]
                        covered=all(any(l["bbox"][0]<=x<=l["bbox"][2] and l["bbox"][1]<=y<=l["bbox"][3] for b in candidates for l in b.get("textlines",[])) for x,y in centers)
                        resolved=bool(candidates) and covered
                        page_diag["components"].append({"indices":sorted(component),"roi":raw_roi,"effective_roi":roi,"candidate_count":len(candidates),"covered_component_words":covered,"resolved":resolved})
                        if resolved: replacements.append((min(component),component,candidates))
                    boxes = rebuild_boxes(original_boxes, replacements)
                page_layout["boxes"]=boxes; item["pages"].append(page_diag)
            result=ns["project"](layout)
        gold=json.loads((root/"gold"/(case+".json")).read_text()); paragraphs=[b["text"] for s in result["sections"] for b in s["blocks"] if b["kind"]=="paragraph"]; text="\n\n".join(paragraphs)
        item.update({"canonical_text":text,"cer":metric(gold["text"],text),"paragraph_count":len(paragraphs),"paragraphs":paragraphs,"ocr_evidence":result.get("ocr_evidence",[])})
        report["cases"][case]=item
    out=Path(sys.argv[1]) if len(sys.argv)>1 else Path("/tmp/regional-retry-probe.json"); out.parent.mkdir(parents=True,exist_ok=True); rendered=json.dumps(report,ensure_ascii=False,indent=2)+"\n"; out.write_text(rendered); print(rendered,end="")
if __name__=="__main__": main()
