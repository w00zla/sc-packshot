packshot bundle (sc-quartermaster), Linux

Contents: packshot (static binary), starbreaker/ (StarBreaker CLI, our fork), the official Blender tarball,
portable/ (Blender config and StarBreaker add-on), cli/render.py, targets.json (target list from the development
DB), setup.sh, check.sh, run.sh, packshot.example.json (cluster settings). The first run start (Enter) asks for live's
target list: Enter takes the targets.json next to it; every set keeps its own (t in view 2 replaces it).

1. Copy the whole folder to the Linux node, e.g. scp -r linux/ user@host:packshot
2. There: bash setup.sh    (unpacks Blender into ./blender and puts portable/ into it)
3. Then:  bash check.sh    (add-on loaded? EEVEE render on Vulkan possible?)
4. For --export: the game's Data.p4k and its build_manifest.id (both from .../StarCitizen/LIVE/) next to run.sh,
   or --p4k "<game folder>/Data.p4k"; StarBreaker is linked against glibc: the node needs at least the glibc the
   bundle script reported (ldd --version).
5. cp packshot.example.json packshot.json and set the same token as on the other nodes
6. ./run.sh --render --export   start with what this node can do now: --render (Blender), --export (Data.p4k),
                                both, or neither (then it only starts runs, stores them and watches). The counts
                                change while it runs: + / - Blender, < / > exports (default ¾ and ¼ of the cores)
   ./run.sh --render --serve    no TUI: works for the other nodes' runs and prints lines (e.g. as a service)
   ./run.sh --help              all options

The nodes find each other by UDP broadcast on port 47950 and connect by TCP on the same port (a firewall must let
both in from the local network, e.g. firewall-cmd --add-port=47950/tcp --add-port=47950/udp --permanent and
--reload); nodes the broadcast does not reach go into "peers" in packshot.json. Every node can start a run, here or on
another node (key n in view 1): that node is the run's head node and holds its runs and sets. The head node gives the
state: every node needs its packshot, Blender, add-on and StarBreaker, the exporting nodes the same Data.p4k as each
other, else no run starts: copy a new bundle to every node after an update.
