/* Root-relative filesystem and sendfile offsets; each run needs an empty cwd. */
static long call(long n,long a,long b,long c,long d) {
    register long r10 __asm__("r10")=d;
    long r;
    __asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10):"rcx","r11","memory");
    return r;
}
#define CHECK(x) do { if(!(x)) { call(231,__LINE__,0,0,0); __builtin_unreachable(); } } while(0)
__attribute__((noreturn)) void _start(void) {
    CHECK(call(83,(long)"fixture",0700,0,0)==0);
    long in=call(2,(long)"fixture/source",2|64|128,0600,0);
    CHECK(in>=0);
    CHECK(call(1,in,(long)"abcdef",6,0)==6);
    CHECK(call(8,in,0,0,0)==0);
    long out=call(2,(long)"fixture/destination",2|64|128,0600,0);
    CHECK(out>=0);
    unsigned long fsinfo[15];
    CHECK(call(138,in,(long)fsinfo,0,0)==0 && fsinfo[0]!=0 && fsinfo[1]>0);
    long directory=call(2,(long)"fixture",0x10000,0,0);
    CHECK(directory>=0);
    CHECK(call(81,-100,0,0,0)==-9 && call(81,in,0,0,0)==-20);
    long previous=call(2,(long)".",0x10000,0,0); CHECK(previous>=0);
    CHECK(call(81,directory,0,0,0)==0);
    long childpath=call(2,(long)"source",0,0,0); CHECK(childpath>=0);
    CHECK(call(3,childpath,0,0,0)==0);
    CHECK(call(81,previous,0,0,0)==0 && call(3,previous,0,0,0)==0);
    long relative=call(257,directory,(long)"source",0,0);
    CHECK(relative>=0 && call(3,relative,0,0,0)==0);
    char entries[128];
    CHECK(call(217,directory,(long)entries,8,0)==-22);
    for(int pass=0;pass<2;pass++) {
        unsigned seen=0; long size;
        while((size=call(217,directory,(long)entries,sizeof entries,0))>0) {
            for(long pos=0;pos<size;) {
                CHECK(pos+20<=size);
                unsigned record=(unsigned char)entries[pos+16] | ((unsigned char)entries[pos+17]<<8);
                CHECK(record>=24 && pos+record<=size);
                const char *name=entries+pos+19;
                if(name[0]=='.' && name[1]==0) seen|=1;
                if(name[0]=='.' && name[1]=='.' && name[2]==0) seen|=2;
                if(name[0]=='s' && name[1]=='o' && name[5]=='e' && name[6]==0) seen|=4;
                if(name[0]=='d' && name[1]=='e' && record>=32 && name[10]=='n' && name[11]==0) seen|=8;
                pos+=record;
            }
        }
        CHECK(size==0 && seen==15);
        CHECK(call(8,directory,0,0,0)==0);
    }
    CHECK(call(3,directory,0,0,0)==0);
    long offset=2;
    CHECK(call(40,out,in,(long)&offset,3)==3 && offset==5);
    CHECK(call(8,in,0,1,0)==0);
    CHECK(call(40,out,in,0,2)==2);
    CHECK(call(8,in,0,1,0)==2);
    CHECK(call(8,out,0,0,0)==0);
    char b[8]={0};
    CHECK(call(0,out,(long)b,8,0)==5);
    CHECK(b[0]=='c' && b[2]=='e' && b[3]=='a' && b[4]=='b');
    CHECK(call(3,in,0,0,0)==0 && call(3,out,0,0,0)==0);
    CHECK(call(86,(long)"fixture/source",(long)"fixture/hard",0,0)==0);
    CHECK(call(88,(long)"source",(long)"fixture/link",0,0)==0);
    CHECK(call(82,(long)"fixture/link",(long)"fixture/moved",0,0)==0);
    CHECK(call(89,(long)"fixture/moved",(long)b,8,0)==6);
    CHECK(b[0]=='s' && b[5]=='e');
    CHECK(call(90,(long)"fixture/source",0640,0,0)==0);
    CHECK(call(87,(long)"fixture/moved",0,0,0)==0);
    CHECK(call(87,(long)"fixture/hard",0,0,0)==0);
    CHECK(call(87,(long)"fixture/source",0,0,0)==0);
    CHECK(call(87,(long)"fixture/destination",0,0,0)==0);
    CHECK(call(84,(long)"fixture",0,0,0)==0);
    call(231,0,0,0,0);
    __builtin_unreachable();
}
