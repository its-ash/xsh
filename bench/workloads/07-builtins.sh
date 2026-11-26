# test, [[ ]], case, printf -v free builtins
i=0
m=0
while [ $i -lt 30000 ]; do
  case $((i % 4)) in
    0) [[ $i == *0 ]] && m=$((m + 1)) ;;
    1) test $i -gt 5 && m=$((m + 2)) ;;
    *) m=$((m + 3)) ;;
  esac
  i=$((i + 1))
done
echo $m
