#!/usr/bin/env bash
# Markdown tables from a ci/stt-bench.sh log (BENCH_ROW / BENCH_MEM lines): bash ci/stt-bench-table.sh <log>
set -euo pipefail
grep -a '^BENCH_' "$1" | awk '
function kv(s,k,  m){ match(s, k "=[^ ]*"); return substr(s, RSTART+length(k)+1, RLENGTH-length(k)-1) }
/^BENCH_ROW/ { c=kv($0,"engine") "/" kv($0,"model"); if(!(c in seen)){seen[c]=1; order[++n]=c}
  key=c SUBSEP kv($0,"cond") SUBSEP kv($0,"stage") SUBSEP kv($0,"subset"); w[key]=kv($0,"wer"); r[key]=kv($0,"rtf") }
/^BENCH_MEM/ { c=kv($0,"engine") "/" kv($0,"model"); mem[c]=kv($0,"peak_rss_mb"); ld[c]=kv($0,"load_s") }
function k(c,cond,st,ss){ return c SUBSEP cond SUBSEP st SUBSEP ss }
END {
  print "| engine/model | clean live | clean final | noise live | noise final | crosstalk live | crosstalk final |"
  print "|---|---|---|---|---|---|---|"
  for(i=1;i<=n;i++){ c=order[i]; printf "| %s", c
    split("clean noise10db crosstalk5db", cs, " "); for(j=1;j<=3;j++) for(s=1;s<=2;s++){ st=(s==1?"live":"final"); printf " | %.1f%%", 100*w[k(c,cs[j],st,"all")] }
    print " |" }
  print ""
  print "| engine/model | clean final, dev-clean | clean final, dev-other | RTF live (clean) | RTF final (clean) | peak RSS MB | model load s |"
  print "|---|---|---|---|---|---|---|"
  for(i=1;i<=n;i++){ c=order[i]
    printf "| %s | %.1f%% | %.1f%% | %.3f | %.3f | %s | %s |\n", c, 100*w[k(c,"clean","final","dev-clean")], 100*w[k(c,"clean","final","dev-other")], r[k(c,"clean","live","all")], r[k(c,"clean","final","all")], mem[c], ld[c] }
}'
