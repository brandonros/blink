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
