# pipelines of external commands
i=0
while [ $i -lt 300 ]; do
  echo "a b c" | /usr/bin/tr a-z A-Z | /bin/cat > /dev/null
  i=$((i + 1))
done
echo done
