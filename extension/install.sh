cargo b

DIR=""
if [ -d "../target" ]; then
    DIR="../target"
else
    DIR="target"
fi

EXTENSION="$DIR/debug/libextension.so"
mv "$EXTENSION" "/etc/pterodactyl/extensions/"
