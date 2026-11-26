# builtin output with redirection
i=0
while [ $i -lt 30000 ]; do
  echo "line $i" > /dev/null
  i=$((i + 1))
done
echo done
