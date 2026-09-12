"""Score canonical worker output without using gold to select or reorder text."""
import argparse, json, subprocess, sys, unicodedata
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
    config={"enabled":True,"engine_path":"/usr/bin/tesseract","tessdata_path":"/usr/share/tesseract-ocr/5/tessdata","languages":["chi_sim"],"operator_revision":"1","dpi":300,"oem":1,"psm":3,"detector_version":"pdf-layout/v1","protocol_version":"pdf-layout/v1"}
    ns={}; exec(a.worker.read_text(),ns); identity={"config":config,"dependencies":ns["fingerprint_dependencies"](config),"sha256":"quality"}; out={}
    failures=[]
    for case in ("F02","F06","F07","F08","F14"):
        config["languages"] = ["chi_sim"] if case == "F07" else (["eng","chi_sim"] if case == "F08" else ["eng"])
        identity={"config":config,"dependencies":ns["fingerprint_dependencies"](config),"sha256":"quality"}
        cmd=[sys.executable,"-I","-c",a.worker.read_text(),"2000","134217728","16000000",json.dumps(config),json.dumps(identity)]
        raw=(a.fixtures/"pdf"/(case+".pdf")).read_bytes(); result=json.loads(subprocess.run(cmd,input=raw,stdout=subprocess.PIPE,stderr=subprocess.PIPE,check=True,timeout=90).stdout)
        text="\n\n".join(b["text"] for s in result["sections"] for b in s["blocks"] if b["kind"]=="paragraph")
        gold=json.loads((a.fixtures/"gold"/(case+".json")).read_text())
        wer = None if case == "F08" else metric(gold["text"],text,True)
        out[case]={"canonical_text":text,"cer":metric(gold["text"],text),"english_wer":wer,"thresholds":{"cer":0.02 if case in ("F07","F08") else 0.01,"wer":0.03}}
        if out[case]["cer"]["rate"] is None or out[case]["cer"]["rate"] > out[case]["thresholds"]["cer"] or (wer is not None and wer["rate"] > out[case]["thresholds"]["wer"]): failures.append(case)
    out["status"]={"failures":failures,"f08_english_wer":"not_implemented_reliable_bilingual_token_split"}
    a.output.write_text(json.dumps(out,ensure_ascii=False,indent=2)+"\n")
    if failures: raise SystemExit("quality thresholds failed: " + ",".join(failures))
if __name__=="__main__": main()
