/* Guest thread names: truncation, page boundaries, faults and fork isolation. */
static long call(long n,long a,long b,long c,long d,long e,long f) {register long r10 __asm__("r10")=d;register long r8 __asm__("r8")=e;register long r9 __asm__("r9")=f;long r;__asm__ volatile("syscall":"=a"(r):"a"(n),"D"(a),"S"(b),"d"(c),"r"(r10),"r"(r8),"r"(r9):"rcx","r11","memory");return r;}
#define C(n,a,b,c,d) call(n,(long)(a),(long)(b),(long)(c),(long)(d),0,0)
#define CHECK(x) do {if(!(x)){C(231,__LINE__,0,0,0);__builtin_unreachable();}} while(0)
static void check_name(const char *expected) {char value[16];CHECK(C(157,16,value,0,0)==0);for(int i=0;i<16;i++){CHECK(value[i]==expected[i]);if(!value[i])return;}}
__attribute__((noreturn)) void _start(void) {
    /* Host bounding sets differ; validate the query ABI, not host grants. */
    long bound=C(157,23,0,0,0); CHECK(bound==0 || bound==1);
    bound=C(157,23,32,0,0); CHECK(bound==0 || bound==1);
    CHECK(C(157,23,41,0,0)==-22);
    CHECK(C(157,23,-1,0,0)==-22);
    CHECK(C(157,15,"0123456789abcdefghijkl",0,0)==0);check_name("0123456789abcde");
    long page=call(9,0,8192,3,0x22,-1,0);CHECK(page>0 && C(10,page+4096,4096,0,0)==0);
    char *end=(char*)(page+4096);end[-1]=0;
    CHECK(C(157,15,end-1,0,0)==0);check_name("");
    for(int i=1;i<=15;i++)end[-i]='x';
    CHECK(C(157,15,end-15,0,0)==0);check_name("xxxxxxxxxxxxxxx");
    CHECK(C(157,15,end-14,0,0)==-14);check_name("xxxxxxxxxxxxxxx");
    CHECK(C(157,16,end-15,0,0)==-14);
    CHECK(C(157,15,"parent",0,0)==0);
    long child=C(57,0,0,0,0);CHECK(child>=0);
    if(child==0){check_name("parent");CHECK(C(157,15,"child",0,0)==0);check_name("child");C(231,0,0,0,0);__builtin_unreachable();}
    int status;CHECK(C(61,child,&status,0,0)==child && status==0);check_name("parent");
    CHECK(C(11,page,8192,0,0)==0);
    C(231,0,0,0,0);__builtin_unreachable();
}
