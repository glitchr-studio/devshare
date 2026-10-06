# Puts DevShare's commands on the PATH of the shell that sources this file:
#
#   source ~/Sites/devshare/env.sh
#
# Add that line to ~/.bash_profile or ~/.zshrc to have them in every shell.
# The commands themselves are put in bin/ by `make install`.

# The folder of this file, in bash and in zsh.
if [ -n "${BASH_SOURCE:-}" ]; then
    _devshare_home=${BASH_SOURCE[0]}
elif [ -n "${ZSH_VERSION:-}" ]; then
    eval '_devshare_home=${(%):-%x}'
else
    _devshare_home=$0
fi
_devshare_home=$(cd "$(dirname "$_devshare_home")" && pwd)

# Once, however many times the file is sourced.
case ":$PATH:" in
    *":$_devshare_home/bin:"*) ;;
    *) PATH="$_devshare_home/bin:$PATH" ;;
esac
export PATH

if [ ! -x "$_devshare_home/bin/devshare" ]; then
    echo "devshare is not built yet: run \"make install\" in $_devshare_home" >&2
fi
unset _devshare_home
