"""Score canonical worker output without using gold to select or reorder text."""
import argparse, json, subprocess, sys, unicodedata, re
from pathlib import Path

def norm(s): return " ".join(unicodedata.normalize("NFC", s).split())
def distance(a,b):
    row=list(range(len(b)+1))
    for i,x in enumerate(a,1):
        nxt=[i]
        for j,y in enumerate(b,1): nxt.append(min(nxt[-1]+1,row[j]+1,row[j-1]+(x!=y)))
        row=nxt
    return row[-1]
def metric(ref, got, words=False):
    a,b=norm(ref),norm(got)
    if words: a,b=a.split(),b.split()
    d=distance(a,b); return {"errors":d,"denominator":len(a),"rate":d/len(a) if a else None}
def main():
    p=argparse.ArgumentParser(); p.add_argument("--fixtures",type=Path,default=Path("tests/fixtures/scanned_pdf")); p.add_argument("--worker",type=Path,default=Path("src/parsing/pdf_layout_worker.py")); p.add_argument("--output",type=Path,required=True); a=p.parse_args()
    config={"enabled":True,"engine_path":"/usr/bin/tesseract","tessdata_path":"/usr/share/tesseract-ocr/5/tessdata","languages":["eng"],"operator_revision":"1","dpi":300,"oem":1,"psm":3,"detector_version":"pdf-layout/v1","protocol_version":"pdf-layout/v1"}
    ns={}; exec(a.worker.read_text(),ns); out={}
    failures=[]
    for case in ("F02","F06","F07","F08","F14"):
        config["languages"] = ["chi_sim"] if case == "F07" else (["eng","chi_sim"] if case == "F08" else ["eng"])
        try:
            identity={"config":config,"dependencies":ns["fingerprint_dependencies"](config),"sha256":"quality"}
            cmd=[sys.executable,"-I","-c",a.worker.read_text(),"2000","134217728","16000000",json.dumps(config),json.dumps(identity)]
            raw=(a.fixtures/"pdf"/(case+".pdf")).read_bytes(); proc=subprocess.run(cmd,input=raw,stdout=subprocess.PIPE,stderr=subprocess.PIPE,timeout=90,check=True); result=json.loads(proc.stdout)
            paragraphs=[b["text"] for s in result["sections"] for b in s["blocks"] if b["kind"]=="paragraph"]; text="\n\n".join(paragraphs); gold=json.loads((a.fixtures/"gold"/(case+".json")).read_text())
            ambiguous=[p for p in paragraphs if re.search(r"[A-Za-z]",p) and re.search(r"[\u3400-\u9fff]",p)]
            if case == "F08":
                expected="\n\n".join(p["text"] for p in gold["paragraphs"] if p["font"]=="goldeng")
                actual="\n\n".join(p for p in paragraphs if re.search(r"[A-Za-z]",p) and not re.search(r"[\u3400-\u9fff]",p)); wer=metric(expected,actual,True)
            else: wer = None if case == "F07" else metric(gold["text"],text,True)
            out[case]={"canonical_text":text,"cer":metric(gold["text"],text),"english_wer":wer,"ambiguous_mixed_paragraphs":ambiguous,"thresholds":{"cer":0.02 if case in ("F07","F08") else 0.01,"wer":None if case=="F07" else 0.03}}
            if out[case]["cer"]["rate"] is None or out[case]["cer"]["rate"] > out[case]["thresholds"]["cer"] or (case == "F08" and ambiguous) or (wer is not None and wer["rate"] > out[case]["thresholds"]["wer"]): failures.append(case)
        except Exception as error:
            stderr = getattr(error, "stderr", b"") or b""
            out[case]={"error":str(error),"stderr":stderr.decode(errors="replace")[-4096:]}; failures.append(case)
    out["status"]={"failures":failures,"f07_english_wer":"not_applicable"}
    a.output.write_text(json.dumps(out,ensure_ascii=False,indent=2)+"\n")
    if failures: raise SystemExit("quality thresholds failed: " + ",".join(failures))
if __name__=="__main__": main()
