/* MADV_DONTFORK removes only selected mappings from fork children. */
static long call(long n,long a,long b,long c,long d,long e,long f) {register long r10 __asm__("r10")=d;register long r8 __asm__("r8")=e;register long r9 __asm__("r9")=f;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");return r;}
#define C(n,a,b,c,d) call(n,(long)(a),(long)(b),(long)(c),(long)(d),0,0)
#define CHECK(x) do {if(!(x)){C(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
__attribute__((noreturn)) void _start(void) {
    unsigned char info[112];CHECK(C(99,info,0,0,0)==0 && C(99,1,0,0,0)==-14);
    CHECK(*(long*)info>=0 && *(unsigned long*)(info+32)>0);
    CHECK(*(unsigned long*)(info+40)<=*(unsigned long*)(info+32));
    CHECK(*(unsigned short*)(info+80)>0 && *(unsigned*)(info+104)>0);
    long base=call(9,0,8192,3,0x22,-1,0);CHECK(base>0);
    *(volatile long*)base=41;*(volatile long*)(base+4096)=42;
    CHECK(C(28,base+1,4096,10,0)==-22);
    CHECK(C(28,base,1,10,0)==0 && C(10,base,4096,0,0)==0);
    for(int inherit=0;inherit<2;inherit++) {
        long child=C(57,0,0,0,0);CHECK(child>=0);
        if(child==0) {
            CHECK(C(10,base,4096,3,0)==(inherit?0:-12));
            CHECK(*(volatile long*)(base+4096)==42);
            if(inherit)CHECK(*(volatile long*)base==41);
            *(volatile long*)(base+4096)=99;
            C(231,0,0,0,0);__builtin_unreachable();
        }
        int status;CHECK(C(61,child,&status,0,0)==child && status==0);
        CHECK(*(volatile long*)(base+4096)==42);
        CHECK(C(28,base,4096,11,0)==0);
    }
    CHECK(C(10,base,4096,3,0)==0 && *(volatile long*)base==41);
    CHECK(C(11,base,8192,0,0)==0);
    CHECK(C(28,base,4096,10,0)==-12 && C(28,base,0,10,0)==0);
    /* Reserve far more virtual space than the interpreter's backing budget. */
    long reserve=call(9,0,512L*1024*1024,0,0x22,-1,0);CHECK(reserve>0);
    CHECK(C(10,reserve,4096,3,0)==0);
    *(volatile long*)reserve=123;
    CHECK(C(10,reserve,4096,0,0)==0 && C(10,reserve,4096,3,0)==0);
    CHECK(*(volatile long*)reserve==123);
    CHECK(C(28,reserve,1,4,0)==0 && *(volatile long*)reserve==0);
    *(volatile long*)reserve=456;
    CHECK(C(10,reserve,4096,0,0)==0 && C(28,reserve,4096,4,0)==0);
    CHECK(C(10,reserve,4096,3,0)==0 && *(volatile long*)reserve==0);
    CHECK(C(28,reserve+1,4096,4,0)==-22);
    CHECK(C(11,reserve,512L*1024*1024,0,0)==0);
    CHECK(C(28,reserve,4096,4,0)==-12);
    C(231,0,0,0,0);__builtin_unreachable();
}
