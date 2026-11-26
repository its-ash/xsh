# fork/exec an external command
i=0
while [ $i -lt 500 ]; do
  /usr/bin/true
  i=$((i + 1))
done
echo done
